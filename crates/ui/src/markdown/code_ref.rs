//! File references in inline code: the `` `src/a.rs:12-20` `` an agent
//! cites when it explains code. The transcript shows one as a short chip
//! (file name and lines; the full path on hover, with a preview of the
//! lines) instead of the long path, which otherwise chops a paragraph into
//! fragments.
//!
//! Detection is deliberately narrow — inline code is mostly identifiers,
//! commands and values, and a false positive would hide what the agent
//! wrote. A span is a reference when it is one path-shaped token that names
//! a file and either cites lines (`file.py:128-131`, `a.rs#L7`) or has a
//! directory (`core/context.py`). Bare names (`Cargo.toml`), globs,
//! placeholders (`data/rank_<n>.pt`), URLs and module paths stay code.

use super::parser::{Block, BlockTree, InlineRun};

/// One file reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodeRef {
    /// The path as written, without its line suffix: relative (to the
    /// checkout or to some folder in it) or absolute.
    pub path: String,
    /// The cited lines, 1-based and inclusive; `None` cites the whole file.
    pub lines: Option<(usize, usize)>,
}

impl CodeRef {
    /// The file name: the path's last segment.
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The folder part of the path (`""` for a bare name), without the
    /// trailing slash.
    pub fn dir(&self) -> &str {
        self.path
            .rsplit_once('/')
            .map_or("", |(dir, _)| if dir.is_empty() { "/" } else { dir })
    }

    /// `:12` or `:12-20`; empty for a whole file.
    pub fn line_suffix(&self) -> String {
        match self.lines {
            Some((first, last)) if last > first => format!(":{first}-{last}"),
            Some((first, _)) => format!(":{first}"),
            None => String::new(),
        }
    }
}

/// The file reference an inline code span holds, if it is one.
pub fn detect(code: &str) -> Option<CodeRef> {
    let text = code.trim();
    if text.is_empty()
        || text.len() > 512
        || text.contains("://")
        || !text.chars().all(is_path_char)
    {
        return None;
    }
    let (path, lines) = split_lines(text);
    if path.is_empty() || path.ends_with('/') || path.contains(':') || path.contains('#') {
        return None;
    }
    if path.starts_with('-') || path.contains("//") {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let has_dir = path.contains('/');
    if !(names_file(name) && (lines.is_some() || has_dir)) {
        return None;
    }
    // `./` and `~/` prefixes are fine; a path of only dots is not a file.
    if path
        .split('/')
        .all(|part| part.is_empty() || part.chars().all(|c| c == '.' || c == '~'))
    {
        return None;
    }
    Some(CodeRef {
        path: path.to_owned(),
        lines,
    })
}

/// What a chip shows for each reference of one text element, in order: the
/// file name, or `folder/name` where two references in the element share a
/// name but not a path (`a/mod.rs` and `b/mod.rs` must not read the same),
/// then the line suffix.
pub fn labels(refs: &[&CodeRef]) -> Vec<String> {
    refs.iter()
        .map(|code_ref| {
            let name = code_ref.file_name();
            let clashes = refs.iter().any(|other| {
                other.file_name() == name && other.path != code_ref.path && !other.dir().is_empty()
            });
            let parent = code_ref.dir().rsplit('/').next().unwrap_or_default();
            let shown = if clashes && !parent.is_empty() && parent != "/" {
                format!("{parent}/{name}")
            } else {
                name.to_owned()
            };
            format!("{shown}{}", code_ref.line_suffix())
        })
        .collect()
}

/// Glue before a chip's label, painted over with a file glyph: two no-break
/// spaces in the code font (about one em), so the chip's width — and the
/// line breaking around it — is settled by the text layout alone.
pub const ICON_GLUE: &str = "\u{a0}\u{a0}";

/// Every chip-shown reference in a tree, in document order — the context a
/// bare `file.py:12` is read in (the agent wrote its full path elsewhere in
/// the answer).
pub fn refs_in(tree: &BlockTree) -> Vec<CodeRef> {
    fn runs(out: &mut Vec<CodeRef>, runs: &[InlineRun]) {
        out.extend(
            runs.iter()
                .filter(|run| run.style.code && run.style.link.is_none())
                .filter_map(|run| detect(&run.text)),
        );
    }
    fn walk(out: &mut Vec<CodeRef>, block: &Block) {
        match block {
            Block::Paragraph { runs: r } | Block::Heading { runs: r, .. } => runs(out, r),
            Block::BlockQuote { children } => children.iter().for_each(|b| walk(out, b)),
            Block::List { items, .. } => items.iter().flatten().for_each(|b| walk(out, b)),
            Block::Table { header, rows, .. } => header
                .iter()
                .chain(rows.iter().flatten())
                .for_each(|cell| runs(out, cell)),
            Block::CodeBlock { .. } | Block::Artifact { .. } | Block::Rule => {}
        }
    }
    let mut out = Vec::new();
    for top in &tree.blocks {
        walk(&mut out, &top.block);
    }
    out
}

/// The longer path a reference most likely abbreviates: the one other path
/// among `context` that ends in this reference's path, when exactly one
/// does (`file.py` beside `utils/processors/file.py`).
pub fn expand_with(code_ref: &CodeRef, context: &[CodeRef]) -> Option<String> {
    let tail = format!("/{}", code_ref.path.trim_start_matches("./"));
    let mut longer = context
        .iter()
        .map(|other| other.path.as_str())
        .filter(|path| path.ends_with(&tail));
    let first = longer.next()?;
    longer.all(|path| path == first).then(|| first.to_owned())
}

fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~' | '@' | '+' | ':' | '#')
}

/// `(path, lines)`: a trailing `:12`, `:12-20`, `:12:5` (column dropped),
/// `#L12`, `#L12-L20` or `#L12-20` read as the cited lines.
fn split_lines(text: &str) -> (&str, Option<(usize, usize)>) {
    if let Some((path, fragment)) = text.rsplit_once("#L")
        && let Some(lines) = parse_range(fragment, true)
    {
        return (path, Some(lines));
    }
    let Some((head, last)) = text.rsplit_once(':') else {
        return (text, None);
    };
    // `a.rs:12:5` — a column after the line.
    if let Some((path, line)) = head.rsplit_once(':')
        && is_digits(line)
        && is_digits(last)
        && let Ok(line) = line.parse::<usize>()
        && line > 0
    {
        return (path, Some((line, line)));
    }
    match parse_range(last, false) {
        Some(lines) => (head, Some(lines)),
        None => (text, None),
    }
}

/// `12` or `12-20` (with `L` before the end too in a `#L` fragment). A
/// reversed range reads as its first line.
fn parse_range(text: &str, fragment: bool) -> Option<(usize, usize)> {
    let (first, last) = match text.split_once('-') {
        Some((first, last)) => {
            let last = if fragment {
                last.strip_prefix('L').unwrap_or(last)
            } else {
                last
            };
            (first, Some(last))
        }
        None => (text, None),
    };
    if !is_digits(first) {
        return None;
    }
    let first: usize = first.parse().ok().filter(|&n| n > 0)?;
    let last = match last {
        Some(last) if is_digits(last) => last.parse().ok().filter(|&n| n >= first),
        Some(_) => return None,
        None => None,
    };
    Some((first, last.unwrap_or(first)))
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.len() <= 9 && text.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a path segment names a file: `name.ext` with a short extension
/// that has a letter in it (`v1.2` and `3.14` do not), a dotfile, or one of
/// the build files that have no extension.
fn names_file(name: &str) -> bool {
    const BARE: &[&str] = &[
        "Makefile",
        "makefile",
        "GNUmakefile",
        "Dockerfile",
        "Containerfile",
        "Justfile",
        "justfile",
        "Gemfile",
        "Rakefile",
        "Procfile",
        "Vagrantfile",
        "Brewfile",
        "BUILD",
        "WORKSPACE",
        "LICENSE",
        "README",
        "CODEOWNERS",
    ];
    if BARE.contains(&name) {
        return true;
    }
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        // A dotfile (`.env`, `.gitignore`).
        return ext.len() >= 2 && ext.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    }
    (1..=12).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic())
        && !stem.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str, lines: Option<(usize, usize)>) -> Option<CodeRef> {
        Some(CodeRef {
            path: path.into(),
            lines,
        })
    }

    #[test]
    fn cited_files_are_references() {
        assert_eq!(
            detect("mvp_engine/engine/engine.py:394"),
            r("mvp_engine/engine/engine.py", Some((394, 394)))
        );
        assert_eq!(
            detect("utils/checkpointing/processors/file.py:69-73"),
            r("utils/checkpointing/processors/file.py", Some((69, 73)))
        );
        assert_eq!(detect("file.py:128-131"), r("file.py", Some((128, 131))));
        assert_eq!(detect("core/context.py"), r("core/context.py", None));
        assert_eq!(detect("./src/a.rs:12:5"), r("./src/a.rs", Some((12, 12))));
        assert_eq!(detect("/abs/a.rs#L7"), r("/abs/a.rs", Some((7, 7))));
        assert_eq!(detect("a.rs#L7-L9"), r("a.rs", Some((7, 9))));
        assert_eq!(detect("a.rs#L7-9"), r("a.rs", Some((7, 9))));
        assert_eq!(detect("~/x/.env"), r("~/x/.env", None));
        assert_eq!(detect("docker/Dockerfile"), r("docker/Dockerfile", None));
        assert_eq!(detect("Makefile:12"), r("Makefile", Some((12, 12))));
        assert_eq!(detect(" src/lib.rs "), r("src/lib.rs", None));
        // A reversed range keeps its first line.
        assert_eq!(detect("a.rs:20-12"), r("a.rs", Some((20, 20))));
    }

    #[test]
    fn other_code_is_not_a_reference() {
        for code in [
            "",
            "identity()",
            "rank",
            "world_size",
            "Cargo.toml",
            "data/rank_<n>.pt",
            "src/**/*.rs",
            "https://example.com/a.rs",
            "std::fs::read",
            "a.rs:x",
            "a.rs:0",
            "src/",
            "foo/bar",
            "v1.2.3",
            "1/2.5",
            "x.rs:12 y.rs:3",
            "$HOME/a.sh",
            "-rf/a.rs",
            "a//b.rs",
            "../..",
            "self.engine.step",
        ] {
            assert_eq!(detect(code), None, "{code:?}");
        }
    }

    #[test]
    fn bare_names_expand_to_the_one_longer_path_beside_them() {
        let tree = super::super::parser::parse_full(
            "Saved (`utils/processors/file.py:69-73`; `mvp/engine.py:394`).\n\n\
             - matched in `file.py:128-131`\n- see `core/context.py`\n",
        );
        let refs = refs_in(&tree);
        assert_eq!(refs.len(), 4);
        let bare = detect("file.py:128-131").unwrap();
        assert_eq!(
            expand_with(&bare, &refs).as_deref(),
            Some("utils/processors/file.py")
        );
        let context = detect("context.py:1").unwrap();
        assert_eq!(
            expand_with(&context, &refs).as_deref(),
            Some("core/context.py")
        );
        // Two different longer paths: no guess.
        let clash = [detect("a/x.rs").unwrap(), detect("b/x.rs").unwrap()];
        assert_eq!(expand_with(&detect("x.rs:1").unwrap(), &clash), None);
        // Nothing longer: nothing to expand.
        assert_eq!(expand_with(&detect("y.rs:1").unwrap(), &refs), None);
    }

    #[test]
    fn labels_are_names_with_lines_and_clashing_names_keep_a_folder() {
        let a = detect("utils/checkpointing/processors/file.py:69-73").unwrap();
        let b = detect("file.py:128-131").unwrap();
        let c = detect("core/context.py:146-160").unwrap();
        // A bare name most likely means the longer path beside it: no clash.
        assert_eq!(
            labels(&[&a, &b, &c]),
            ["file.py:69-73", "file.py:128-131", "context.py:146-160"]
        );
        let x = detect("a/mod.rs:1").unwrap();
        let y = detect("b/mod.rs").unwrap();
        assert_eq!(labels(&[&x, &y]), ["a/mod.rs:1", "b/mod.rs"]);
        // The same file twice is no clash.
        let z = detect("a/mod.rs:9").unwrap();
        assert_eq!(labels(&[&x, &z]), ["mod.rs:1", "mod.rs:9"]);
    }
}
