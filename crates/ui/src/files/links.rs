//! File links in chat Markdown: the destinations agents write for files
//! (`src/a.rs`, `/abs/a.rs:12`, `a.rs#L12`, `file:///abs/a.rs`, the
//! composer's `cypher-file:` mentions), read into a path and an optional
//! line, and resolved against the chat's checkout so the Files surface can
//! open them. Anything with another scheme (`https:`, `mailto:`…) is not a
//! file link and keeps opening through the system.

/// A file a chat link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLink {
    /// As written, percent-decoded: absolute, or relative to the checkout.
    pub path: String,
    /// 1-based line, from a `:12` suffix or a `#L12` fragment.
    pub line: Option<usize>,
    /// Written with a trailing slash: a folder.
    pub dir: bool,
}

/// The file `url` points at, or `None` when it is not a file link.
pub fn parse(url: &str) -> Option<FileLink> {
    let url = url.trim();
    let rest = if let Some(rest) = url
        .strip_prefix("cypher-file:")
        .or_else(|| url.strip_prefix("zeron-file:"))
    {
        rest
    } else if url.len() >= 7 && url[..7].eq_ignore_ascii_case("file://") {
        let rest = &url[7..];
        rest.strip_prefix("localhost").unwrap_or(rest)
    } else if has_scheme(url) {
        return None;
    } else {
        url
    };
    let (rest, fragment) = match rest.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (rest, None),
    };
    let rest = rest.split_once('?').map_or(rest, |(path, _)| path);
    let mut path = percent_decode(rest).unwrap_or_else(|| rest.to_owned());
    let mut line = fragment.and_then(fragment_line);
    if line.is_none()
        && let Some((head, at)) = split_line_suffix(&path)
    {
        path = head;
        line = Some(at);
    }
    let dir = path.ends_with('/');
    let path = path.trim_end_matches('/').to_owned();
    (!path.is_empty()).then_some(FileLink { path, line, dir })
}

/// `path` inside the checkout at `cwd`, relative to it (`""` is the root),
/// or the absolute path when it lies outside.
pub fn resolve(path: &str, cwd: &str) -> Result<String, String> {
    let root = normalize(cwd);
    let full = if path.starts_with('/') {
        normalize(path)
    } else {
        normalize(&format!("{root}/{path}"))
    };
    if full == root {
        return Ok(String::new());
    }
    match full.strip_prefix(&root) {
        Some(rest) if root == "/" => Ok(rest.to_owned()),
        Some(rest) if rest.starts_with('/') => Ok(rest[1..].to_owned()),
        _ => Err(full),
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

/// Whether `url` starts with a URL scheme (`https:`, `mailto:`) — not a
/// drive letter, and not a `name.rs:12` line suffix.
fn has_scheme(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && line_number(rest.split(':').next().unwrap_or_default()).is_none()
}

/// The line of a `L12`, `L12-L20` or `L12C3` fragment.
fn fragment_line(fragment: &str) -> Option<usize> {
    let digits = fragment
        .strip_prefix('L')
        .or_else(|| fragment.strip_prefix('l'))?;
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse().ok().filter(|&n| n > 0)
}

/// `a.rs:12`, `a.rs:12:5` or `a.rs:12-20` → (`a.rs`, 12).
fn split_line_suffix(path: &str) -> Option<(String, usize)> {
    let (head, last) = path.rsplit_once(':')?;
    let last_line = line_number(last)?;
    let (head, line) = match head.rsplit_once(':') {
        Some((before, prev)) if !before.is_empty() && is_digits(prev) => {
            (before, prev.parse().ok()?)
        }
        _ => (head, last_line),
    };
    (!head.is_empty() && line > 0).then(|| (head.to_owned(), line))
}

/// `12` or `12-20` → 12.
fn line_number(text: &str) -> Option<usize> {
    let first = text
        .split_once('-')
        .map_or(text, |(from, to)| if is_digits(to) { from } else { "" });
    if is_digits(first) {
        first.parse().ok()
    } else {
        None
    }
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn percent_decode(text: &str) -> Option<String> {
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

    fn link(path: &str, line: Option<usize>) -> Option<FileLink> {
        Some(FileLink {
            path: path.trim_end_matches('/').into(),
            line,
            dir: path.ends_with('/'),
        })
    }

    #[test]
    fn file_links_read_their_path_and_line() {
        assert_eq!(parse("src/a.rs"), link("src/a.rs", None));
        assert_eq!(parse("./src/a.rs:12"), link("./src/a.rs", Some(12)));
        assert_eq!(parse("/abs/a.rs:12:5"), link("/abs/a.rs", Some(12)));
        assert_eq!(parse("a.rs:12-20"), link("a.rs", Some(12)));
        assert_eq!(parse("a.rs#L7"), link("a.rs", Some(7)));
        assert_eq!(parse("a.rs#L7-L9"), link("a.rs", Some(7)));
        assert_eq!(parse("guide/x.md#usage"), link("guide/x.md", None));
        assert_eq!(parse("file:///abs/my%20a.rs"), link("/abs/my a.rs", None));
        assert_eq!(parse("file://localhost/abs/a.rs"), link("/abs/a.rs", None));
        assert_eq!(parse("cypher-file:src/lib.rs"), link("src/lib.rs", None));
        assert_eq!(parse("cypher-file:crates/ui/"), link("crates/ui/", None));
        assert_eq!(parse("Cargo.toml"), link("Cargo.toml", None));
    }

    #[test]
    fn other_links_are_not_files() {
        for url in [
            "https://example.com/a.rs",
            "http://localhost:3000",
            "mailto:a@b.c",
            "cypher-session:chat-1",
            "#heading",
            "",
        ] {
            assert_eq!(parse(url), None, "{url}");
        }
    }

    #[test]
    fn paths_resolve_inside_the_checkout_or_not() {
        let cwd = "/repo/app/";
        assert_eq!(resolve("src/a.rs", cwd), Ok("src/a.rs".into()));
        assert_eq!(resolve("./src/../b.rs", cwd), Ok("b.rs".into()));
        assert_eq!(resolve("/repo/app/src/a.rs", cwd), Ok("src/a.rs".into()));
        assert_eq!(resolve("/repo/app", cwd), Ok(String::new()));
        assert_eq!(
            resolve("/repo/application/a.rs", cwd),
            Err("/repo/application/a.rs".into())
        );
        assert_eq!(
            resolve("../other/a.rs", cwd),
            Err("/repo/other/a.rs".into())
        );
        assert_eq!(resolve("/etc/hosts", cwd), Err("/etc/hosts".into()));
    }
}
