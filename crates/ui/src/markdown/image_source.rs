//! Where a chat image's bytes come from: the `src` of a Markdown image
//! (`![alt](src)`), or a reply paragraph that is nothing but an image path.
//!
//! The rules are shared with the iPhone app through one table,
//! `protocol/vectors/image-sources-v1.json`, which both test suites run:
//!
//! - `https://` URLs load from the network (no cookies, no credentials);
//!   plain `http://` is refused as insecure. A URL whose path names a known
//!   non-image file (`.pdf`) is not an image; one without an extension is
//!   tried, and its bytes decide.
//! - `data:image/…;base64,` URIs carry their bytes; another media type is not
//!   an image, and a data URI that is not base64 is unsupported.
//! - Anything else is a path on the session's host — absolute, `~`, relative
//!   `file://` or `cypher-file:`, percent-decoded, with a `#L12`/`:12` suffix or a `?query`
//!   dropped — and must name a png, jpg/jpeg, gif, webp or svg file (any
//!   case). It is read through the engine, so only inside one of the chat's
//!   roots: a relative path is looked up from the chat's folder first, then
//!   from each root in order; an absolute one only in the innermost root
//!   holding it. Each try is a root and the path relative to it.
//! - Every source is capped at [`MAX_BYTES`].

/// The most bytes one image may be (fetched, read or decoded from a data
/// URI); a larger one shows an error instead.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;

/// The image formats the chat shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
}

impl Format {
    /// By file extension, any case (`PNG`, `jpeg`).
    pub fn for_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "gif" => Some(Self::Gif),
            "webp" => Some(Self::Webp),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }

    /// By media type (`image/png`; parameters ignored).
    pub fn for_mime(mime: &str) -> Option<Self> {
        let mime = mime.split(';').next().unwrap_or_default().trim();
        match mime.to_ascii_lowercase().as_str() {
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::Webp),
            "image/svg+xml" => Some(Self::Svg),
            _ => None,
        }
    }

    /// By a path's (or URL path's) extension.
    pub fn for_path(path: &str) -> Option<Self> {
        extension(path).and_then(Self::for_extension)
    }

    /// From the bytes themselves (magic numbers; SVG by its root element).
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            return Some(Self::Jpeg);
        }
        if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            return Some(Self::Gif);
        }
        if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        let head = &bytes[..bytes.len().min(1024)];
        let head = String::from_utf8_lossy(head);
        let head = head.trim_start_matches('\u{feff}').trim_start();
        (head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")))
            .then_some(Self::Svg)
    }

    /// The vectors' name for the format.
    #[cfg(test)]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Svg => "image/svg+xml",
        }
    }
}

/// Why a source shows an error instead of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rejection {
    /// Plain `http://`.
    Insecure,
    /// A data URI that is not base64.
    Unsupported,
}

impl Rejection {
    /// The vectors' name for the reason.
    #[cfg(test)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Insecure => "insecure",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One place to read a local image: a root and the path inside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Candidate {
    pub root: String,
    pub path: String,
}

/// What an image source is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// A file on the session's host: try each candidate in order.
    Local {
        candidates: Vec<Candidate>,
        format: Format,
    },
    /// A file outside every root the chat may read (absolute, normalized).
    Outside {
        path: String,
    },
    Remote {
        url: String,
    },
    /// A `data:` URI; `payload` is its base64.
    Data {
        format: Format,
        payload: String,
    },
    Rejected(Rejection),
    /// Not an image: it stays a link or a file chip.
    NotImage,
}

/// Where local paths resolve: the chat's folder, the roots it may read (in
/// lookup order) and, when known, the host's home folder for `~`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    pub cwd: Option<&'a str>,
    pub roots: &'a [&'a str],
    pub home: Option<&'a str>,
}

/// Whether `src` is an image at all, before any context: what the parser
/// asks to decide between an image block and a link.
pub fn is_image(src: &str) -> bool {
    !matches!(classify(src, &Context::default()), Source::NotImage)
}

/// What `src` is, with local paths resolved in `context`.
pub fn classify(src: &str, context: &Context) -> Source {
    let src = src.trim();
    if src.is_empty() {
        return Source::NotImage;
    }
    if let Some(rest) = strip_prefix_ci(src, "data:") {
        return data_uri(rest);
    }
    if strip_prefix_ci(src, "http://").is_some() {
        return if remote_may_be_image(src) {
            Source::Rejected(Rejection::Insecure)
        } else {
            Source::NotImage
        };
    }
    if let Some(rest) = strip_prefix_ci(src, "https://") {
        if rest.is_empty() || !remote_may_be_image(src) {
            return Source::NotImage;
        }
        return Source::Remote {
            url: src.to_owned(),
        };
    }
    let Some(path) = local_path(src) else {
        return Source::NotImage;
    };
    let Some(format) = Format::for_path(&path) else {
        return Source::NotImage;
    };
    local(&path, format, context)
}

/// The host path a source names, as written but cleaned: `file://`
/// (optionally `localhost`) and `cypher-file:` stripped, `?query` and
/// `#fragment` dropped, percent-escapes decoded and a `:12`, `:12-20` or
/// `:12:5` line suffix dropped. `None` for URLs and other schemes (`https:`,
/// `data:`, `mailto:`) and for an empty source.
pub fn local_path(src: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    let path = if let Some(rest) = strip_prefix_ci(src, "file://") {
        rest.strip_prefix("localhost").unwrap_or(rest)
    } else if let Some(rest) = strip_prefix_ci(src, "cypher-file:") {
        rest
    } else if has_scheme(src) {
        return None;
    } else {
        src
    };
    let path = path.split(['#', '?']).next().unwrap_or_default();
    let path = percent_decode(path).unwrap_or_else(|| path.to_owned());
    let path = strip_line_suffix(&path);
    (!path.is_empty()).then(|| path.to_owned())
}

/// The image a whole reply paragraph names, given as its Markdown source
/// (trimmed): one path or URL, bare, in backticks or in `<…>`, that names a
/// supported image file. `None` for anything else (prose, a link, several
/// tokens, a path to another kind of file, plain `http://`).
pub fn image_line(line: &str) -> Option<String> {
    let inner = line_token(line)?;
    let named = match classify(inner, &Context::default()) {
        Source::Local { .. } | Source::Outside { .. } => true,
        // A bare URL is an image only when its path says so.
        Source::Remote { url } => {
            remote_extension(&url).is_some_and(|ext| Format::for_extension(ext).is_some())
        }
        Source::Data { .. } | Source::Rejected(_) | Source::NotImage => false,
    };
    named.then(|| inner.to_owned())
}

/// The one token a whole paragraph is (trimmed): bare, in backticks or in
/// `<…>`, without whitespace; bare tokens hold none of `* [ ] ( ) < > ` !`.
pub(crate) fn line_token(line: &str) -> Option<&str> {
    let line = line.trim();
    let inner = if let Some(code) = line.strip_prefix('`').and_then(|l| l.strip_suffix('`')) {
        code.trim()
    } else if let Some(auto) = line.strip_prefix('<').and_then(|l| l.strip_suffix('>')) {
        auto.trim()
    } else {
        if line.contains(['*', '[', ']', '(', ')', '<', '>', '`', '!']) {
            return None;
        }
        line
    };
    if inner.is_empty() || inner.contains(char::is_whitespace) || inner.contains('`') {
        return None;
    }
    Some(inner)
}

fn data_uri(rest: &str) -> Source {
    let Some((meta, payload)) = rest.split_once(',') else {
        return Source::NotImage;
    };
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or_default();
    let Some(format) = Format::for_mime(mime) else {
        return Source::NotImage;
    };
    if !parts.any(|part| part.trim().eq_ignore_ascii_case("base64")) {
        return Source::Rejected(Rejection::Unsupported);
    }
    Source::Data {
        format,
        payload: payload.trim().to_owned(),
    }
}

/// An `http(s)` URL may be an image unless its path names another kind of
/// file.
fn remote_may_be_image(url: &str) -> bool {
    match remote_extension(url) {
        Some(ext) => Format::for_extension(ext).is_some(),
        None => true,
    }
}

/// The extension of a URL's last path segment, if it has one.
fn remote_extension(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.split(['?', '#']).next().unwrap_or_default();
    let (_, path) = path.split_once('/')?;
    extension(path)
}

fn local(path: &str, format: Format, context: &Context) -> Source {
    match local_candidates(path, context) {
        Ok(candidates) => Source::Local { candidates, format },
        Err(path) => Source::Outside { path },
    }
}

/// Where a cleaned local path ([`local_path`]) may be read, in lookup order:
/// as images resolve, for any kind of file. `Err` holds the absolute path
/// when it is outside every root the chat may read.
pub fn local_candidates(path: &str, context: &Context) -> Result<Vec<Candidate>, String> {
    let home = context.home.map(|home| home.trim_end_matches('/'));
    let absolute = |path: &str| -> Option<String> {
        if path == "~" || path.starts_with("~/") {
            let rest = &path[1..];
            return Some(match home {
                Some(home) => normalize(&format!("{home}{rest}")),
                // Unknown home: `~` stays the head of the path, and matches
                // roots written the same way.
                None => format!("~{}", normalize(rest).trim_end_matches('/')),
            });
        }
        path.starts_with('/').then(|| normalize(path))
    };
    let roots: Vec<String> = context
        .roots
        .iter()
        .filter_map(|root| absolute(root))
        .collect();
    let mut tries: Vec<String> = Vec::new();
    if let Some(full) = absolute(path) {
        tries.push(full);
    } else {
        let bases = context
            .cwd
            .and_then(absolute)
            .into_iter()
            .chain(roots.iter().cloned());
        for base in bases {
            tries.push(normalize_keeping_home(&format!("{base}/{path}")));
        }
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for full in &tries {
        let inner = roots
            .iter()
            .filter_map(|root| inside(full, root).map(|rel| (root, rel)))
            .max_by_key(|(root, _)| root.len());
        if let Some((root, rel)) = inner {
            let candidate = Candidate {
                root: root.clone(),
                path: rel,
            };
            if !candidate.path.is_empty() && !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    if candidates.is_empty() {
        return Err(tries.into_iter().next().unwrap_or_else(|| path.to_owned()));
    }
    Ok(candidates)
}

/// `full` relative to `root` when inside it.
fn inside(full: &str, root: &str) -> Option<String> {
    if root == "/" {
        return Some(full.trim_start_matches('/').to_owned());
    }
    if full == root {
        return Some(String::new());
    }
    full.strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(str::to_owned)
}

/// [`normalize`], keeping an unexpanded `~` head.
fn normalize_keeping_home(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("~{}", normalize(rest)),
        None => normalize(path),
    }
}

/// An absolute path with `.` and `..` folded away and no trailing slash.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `a.png:12`, `a.png:12-20` or `a.png:12:5` → `a.png`.
fn strip_line_suffix(path: &str) -> &str {
    let mut out = path;
    for _ in 0..2 {
        match out.rsplit_once(':') {
            Some((head, tail))
                if !head.is_empty()
                    && !tail.is_empty()
                    && tail.chars().all(|c| c.is_ascii_digit() || c == '-') =>
            {
                out = head;
            }
            _ => break,
        }
    }
    out
}

pub(crate) fn extension(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let dot = name.rfind('.').filter(|dot| *dot > 0)?;
    Some(&name[dot + 1..])
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    (text.len() >= prefix.len()
        && text.is_char_boundary(prefix.len())
        && text[..prefix.len()].eq_ignore_ascii_case(prefix))
    .then(|| &text[prefix.len()..])
}

/// A URL scheme (`mailto:`, `ftp:`) — not a drive letter or a `:12` suffix.
fn has_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !scheme.contains('.')
}

fn percent_decode(text: &str) -> Option<String> {
    if !text.contains('%') {
        return None;
    }
    let raw = text.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let hex = std::str::from_utf8(raw.get(at + 1..at + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn expect_json(source: &Source) -> Value {
        match source {
            Source::Local { candidates, format } => json!({
                "kind": "local",
                "mime": format.mime(),
                "candidates": candidates
                    .iter()
                    .map(|c| json!({"root": c.root, "path": c.path}))
                    .collect::<Vec<_>>(),
            }),
            Source::Outside { path } => json!({"kind": "outside", "path": path}),
            Source::Remote { url } => json!({"kind": "remote", "url": url}),
            Source::Data { format, .. } => json!({"kind": "data", "mime": format.mime()}),
            Source::Rejected(reason) => json!({"kind": "rejected", "reason": reason.as_str()}),
            Source::NotImage => json!({"kind": "not_image"}),
        }
    }

    #[test]
    fn shared_vectors() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../../../../protocol/vectors/image-sources-v1.json"
        ))
        .unwrap();
        assert_eq!(vectors["version"], 1);
        assert_eq!(vectors["max_bytes"], MAX_BYTES);
        for (ext, mime) in vectors["formats"].as_object().unwrap() {
            let format = Format::for_extension(ext).unwrap_or_else(|| panic!("{ext}"));
            assert_eq!(format.mime(), mime.as_str().unwrap(), "{ext}");
        }
        let cases = vectors["cases"].as_array().unwrap();
        assert!(cases.len() > 20);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let roots: Vec<&str> = case["roots"]
                .as_array()
                .map(|roots| roots.iter().map(|r| r.as_str().unwrap()).collect())
                .unwrap_or_default();
            let context = Context {
                cwd: case["cwd"].as_str(),
                roots: &roots,
                home: case["home"].as_str(),
            };
            let src = case["src"].as_str().unwrap();
            assert_eq!(
                expect_json(&classify(src, &context)),
                case["expect"],
                "{name}"
            );
        }
        let lines = vectors["lines"].as_array().unwrap();
        assert!(lines.len() > 8);
        for case in lines {
            let name = case["name"].as_str().unwrap();
            let line = case["line"].as_str().unwrap();
            assert_eq!(
                image_line(line).as_deref(),
                case["src"].as_str(),
                "{name}: {line}"
            );
        }
    }

    #[test]
    fn bytes_name_their_format() {
        assert_eq!(Format::sniff(b"\x89PNG\r\n\x1a\nxxxx"), Some(Format::Png));
        assert_eq!(Format::sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some(Format::Jpeg));
        assert_eq!(Format::sniff(b"GIF89a...."), Some(Format::Gif));
        assert_eq!(Format::sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::Webp));
        assert_eq!(
            Format::sniff(b"<?xml version=\"1.0\"?>\n<svg xmlns=\"\"/>"),
            Some(Format::Svg)
        );
        assert_eq!(Format::sniff(b"  <svg/>"), Some(Format::Svg));
        assert_eq!(Format::sniff(b"BM\0\0"), None);
        assert_eq!(Format::sniff(b"%PDF-1.7"), None);
    }

    #[test]
    fn the_parser_asks_without_context() {
        assert!(is_image("shots/a.png"));
        assert!(is_image("http://x/a.png"));
        assert!(is_image("https://x/render?id=3"));
        assert!(!is_image("notes.pdf"));
        assert!(!is_image("https://x/doc.pdf"));
        assert!(!is_image("mailto:a@b.c"));
    }
}
