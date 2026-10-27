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
    /// The last line of a cited range (`:12-20`, `#L12-L20`); `None` for
    /// a single line.
    pub end_line: Option<usize>,
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
    let mut lines = fragment.and_then(fragment_lines);
    if lines.is_none()
        && let Some((head, at)) = split_line_suffix(&path)
    {
        path = head;
        lines = Some(at);
    }
    let dir = path.ends_with('/');
    let path = path.trim_end_matches('/').to_owned();
    (!path.is_empty()).then_some(FileLink {
        path,
        line: lines.map(|(first, _)| first),
        end_line: lines.and_then(|(first, last)| (last > first).then_some(last)),
        dir,
    })
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
        && line_range(rest.split(':').next().unwrap_or_default()).is_none()
}

/// The lines of a `L12`, `L12-L20`, `L12-20` or `L12C3` fragment.
fn fragment_lines(fragment: &str) -> Option<(usize, usize)> {
    let leading_number = |text: &str| -> Option<usize> {
        let digits = text.strip_prefix('L').or_else(|| text.strip_prefix('l'))?;
        let end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        digits[..end].parse().ok().filter(|&n| n > 0)
    };
    let first = leading_number(fragment)?;
    let last = fragment
        .split_once('-')
        .and_then(|(_, to)| leading_number(to).or_else(|| to.parse().ok()))
        .filter(|&last| last >= first)
        .unwrap_or(first);
    Some((first, last))
}

/// `a.rs:12`, `a.rs:12:5` or `a.rs:12-20` → (`a.rs`, (12, 12 or 20)).
fn split_line_suffix(path: &str) -> Option<(String, (usize, usize))> {
    let (head, last) = path.rsplit_once(':')?;
    let last_lines = line_range(last)?;
    let (head, lines) = match head.rsplit_once(':') {
        Some((before, prev)) if !before.is_empty() && is_digits(prev) => {
            let line = prev.parse().ok()?;
            (before, (line, line))
        }
        _ => (head, last_lines),
    };
    (!head.is_empty() && lines.0 > 0).then(|| (head.to_owned(), lines))
}

/// `12` → (12, 12); `12-20` → (12, 20). A reversed range keeps its start.
fn line_range(text: &str) -> Option<(usize, usize)> {
    let (first, last) = match text.split_once('-') {
        Some((from, to)) if is_digits(to) => (from, Some(to)),
        Some(_) => return None,
        None => (text, None),
    };
    if !is_digits(first) {
        return None;
    }
    let first: usize = first.parse().ok()?;
    let last = last
        .and_then(|last| last.parse().ok())
        .filter(|&last| last >= first)
        .unwrap_or(first);
    Some((first, last))
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
            end_line: None,
            dir: path.ends_with('/'),
        })
    }

    fn range(path: &str, first: usize, last: usize) -> Option<FileLink> {
        Some(FileLink {
            end_line: Some(last),
            ..link(path, Some(first))?
        })
    }

    #[test]
    fn file_links_read_their_path_and_line() {
        assert_eq!(parse("src/a.rs"), link("src/a.rs", None));
        assert_eq!(parse("./src/a.rs:12"), link("./src/a.rs", Some(12)));
        assert_eq!(parse("/abs/a.rs:12:5"), link("/abs/a.rs", Some(12)));
        assert_eq!(parse("a.rs:12-20"), range("a.rs", 12, 20));
        assert_eq!(parse("a.rs:20-12"), link("a.rs", Some(20)));
        assert_eq!(parse("a.rs#L7"), link("a.rs", Some(7)));
        assert_eq!(parse("a.rs#L7-L9"), range("a.rs", 7, 9));
        assert_eq!(parse("a.rs#L7-9"), range("a.rs", 7, 9));
        assert_eq!(parse("a.rs#L7C3"), link("a.rs", Some(7)));
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
