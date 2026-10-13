//! ```` ```diff ```` fences rendered by the Changes pane's diff rows — accent
//! bars, row washes, markers, line numbers when the hunks carry them — so a
//! diff the model writes reads like the diff a tool call produced.
//!
//! Syntax colors come from ONE highlighted document per fence: every code
//! line's text, in render order ([`highlight_source`]). Old and new lines
//! sit side by side in that document, which Tree-sitter tolerates; line `i`
//! of the result colors the `i`th rendered code line.

use std::borrow::Cow;

use cypher_syntax::LanguageId;
use gpui::{AnyElement, SharedString, div, prelude::*, px};

use crate::changes::{
    FileDiff, LineKind, diff_line_row, file_notices, gutter_width, hunk_header_row, notice_row,
    parse_fence_diff,
};

use super::render::{BlockCtx, CodeHighlight, copy_button, fence_frame, horizontal_scroller};
use crate::kit::theme::MonoStyled;

/// Fence tags that render as a diff.
pub fn is_diff_fence(language: Option<&str>) -> bool {
    language.is_some_and(|tag| {
        matches!(
            tag.to_ascii_lowercase().as_str(),
            "diff" | "patch" | "udiff"
        )
    })
}

/// The language a file's lines are colored as, from its header path.
fn file_language(file: &FileDiff) -> Option<LanguageId> {
    (!file.path.is_empty())
        .then(|| cypher_syntax::language_for_path(&file.path))
        .flatten()
}

/// The fence's language: the first file header that names one. Chat diffs
/// without file headers stay uncolored rather than guessing.
fn fence_language(files: &[FileDiff]) -> Option<LanguageId> {
    files.iter().find_map(file_language)
}

/// Every code line of `files`, in render order (meta lines excluded).
fn code_lines(files: &[FileDiff]) -> impl Iterator<Item = &str> {
    files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind != LineKind::Meta)
        .map(|line| line.text.as_str())
}

fn highlight_source(files: &[FileDiff]) -> String {
    code_lines(files).collect::<Vec<_>>().join("\n")
}

/// What the syntax pass tokenizes for a fence: the code under its tag, or
/// for a diff fence the merged code lines under the language its file
/// headers name. `None` when there is nothing to color.
pub fn highlight_input<'a>(
    language: Option<&str>,
    code: &'a str,
) -> Option<(LanguageId, Cow<'a, str>)> {
    if is_diff_fence(language) {
        let files = parse_fence_diff(code);
        let lang = fence_language(&files)?;
        return Some((lang, Cow::Owned(highlight_source(&files))));
    }
    let lang = cypher_syntax::language_for_alias(language?)?;
    Some((lang, Cow::Borrowed(code)))
}

/// In-chat find matches, counted the way [`render`] builds its selectable
/// text: one element per code line, markers excluded.
pub fn count_matches(code: &str, query: &str) -> usize {
    code_lines(&parse_fence_diff(code))
        .map(|line| crate::markdown::find::count_matches(line, query))
        .sum()
}

/// `+N −M`, in the diff hues; omitted sides stay out.
fn stats(additions: u32, deletions: u32, theme: &crate::kit::theme::Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap(px(6.0))
        .when(additions > 0, |el| {
            el.child(
                div()
                    .text_color(theme.diff_add)
                    .child(SharedString::from(format!("+{additions}"))),
            )
        })
        .when(deletions > 0, |el| {
            el.child(
                div()
                    .text_color(theme.diff_del)
                    .child(SharedString::from(format!("−{deletions}"))),
            )
        })
}

/// A path label with the file's counts beside it.
fn file_label(label: String, file_stats: (u32, u32), ctx: BlockCtx) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(SharedString::from(label))
        .child(stats(file_stats.0, file_stats.1, ctx.theme))
}

pub fn render(code: &str, ctx: BlockCtx, highlight: CodeHighlight) -> AnyElement {
    let BlockCtx {
        ix, opts, theme, ..
    } = ctx;
    let files = parse_fence_diff(code);
    let language = fence_language(&files);
    let totals = files.iter().fold((0, 0), |(add, del), file| {
        (add + file.additions, del + file.deletions)
    });
    // One file names the fence; several get a strip each and the header
    // keeps the tag.
    let single = files.len() == 1;
    let header_label = match files.first() {
        Some(file) if single && !file.path.is_empty() => file.path.clone(),
        _ => "diff".to_string(),
    };
    let header = file_label(header_label, totals, ctx).into_any_element();

    let mut rows: Vec<AnyElement> = Vec::new();
    // Index into the highlighted document AND the per-line selection key.
    let mut code_line = 0usize;
    for file in &files {
        if !single {
            rows.push(file_strip(file, ctx));
        }
        for notice in file_notices(file) {
            rows.push(notice_row(notice, theme));
        }
        // Lines carry numbers only under numbered hunk headers.
        let gutter = (file.max_line > 0).then(|| gutter_width(file));
        // Mixed-language fences color only the fence language's files.
        let colored = file_language(file).is_none_or(|lang| Some(lang) == language);
        for hunk in &file.hunks {
            if !hunk.header.is_empty() {
                rows.push(hunk_header_row(&hunk.header, theme));
            }
            for line in &hunk.lines {
                if line.kind == LineKind::Meta {
                    rows.push(diff_line_row(line, &[], theme, gutter, None, true));
                    continue;
                }
                let spans = highlight
                    .filter(|_| colored)
                    .and_then(|lines| lines.get(code_line))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                // `-t{element}` keeps the key on its row for comments/quotes.
                let key = format!("{}-t{ix}l{code_line}", opts.row_key);
                let select = opts
                    .selection
                    .as_ref()
                    .map(|selection| (opts.scope, key.as_str(), selection));
                rows.push(diff_line_row(line, spans, theme, gutter, select, true));
                code_line += 1;
            }
        }
    }

    let scroll_id: SharedString = format!("{}-diff{ix}", opts.row_key).into();
    fence_frame(ctx, Some(header))
        .child(
            horizontal_scroller(scroll_id)
                .py(px(4.0))
                .flex()
                .flex_col()
                // The column sizes to its widest line, never narrower than
                // the panel, so row washes span the scrolled width.
                .items_start()
                .child(
                    div()
                        .min_w_full()
                        .flex()
                        .flex_col()
                        .cursor_text()
                        .children(rows),
                ),
        )
        .children(copy_button(code, ctx))
        .into_any_element()
}

/// The per-file strip of a multi-file fence.
fn file_strip(file: &FileDiff, ctx: BlockCtx) -> AnyElement {
    let label = if file.path.is_empty() {
        "diff".to_string()
    } else {
        file.path.clone()
    };
    file_label(label, (file.additions, file.deletions), ctx)
        .h(px(26.0))
        .px(px(super::render::CODE_PADDING_X))
        .mono(ctx.theme)
        .text_size(px(11.0))
        .text_color(ctx.theme.text_muted)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT_DIFF: &str = concat!(
        "fingerprint = {\n",
        "-        \"name\": a,\n",
        "         \"chat\": b,\n",
        "@@ slow path\n",
        "+        if x:\n",
        "+            y = 1\n",
        "\\ No newline at end of file",
    );

    #[test]
    fn diff_tags_are_recognized() {
        assert!(is_diff_fence(Some("diff")));
        assert!(is_diff_fence(Some("Patch")));
        assert!(!is_diff_fence(Some("python")));
        assert!(!is_diff_fence(None));
    }

    #[test]
    fn chat_diffs_parse_into_one_unnumbered_file() {
        let files = parse_fence_diff(CHAT_DIFF);
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!((file.additions, file.deletions, file.max_line), (2, 1, 0));
        assert_eq!(file.hunks.len(), 2);
        assert_eq!(file.hunks[0].header, "");
        assert_eq!(file.hunks[1].header, "@@ slow path");
        let first = &file.hunks[0].lines[0];
        assert_eq!(first.kind, LineKind::Context);
        // An unprefixed line keeps its text whole.
        assert_eq!(first.text, "fingerprint = {");
        assert_eq!(file.hunks[0].lines[2].text, "        \"chat\": b,");
        assert_eq!(file.hunks[1].lines[2].kind, LineKind::Meta);
    }

    #[test]
    fn git_patches_keep_paths_and_numbers() {
        let files = parse_fence_diff(
            "diff --git a/src/a.py b/src/a.py\nindex 1..2 100644\n--- a/src/a.py\n+++ b/src/a.py\n\
@@ -10,3 +10,3 @@ def f():\n ctx\n-old\n+new\n--- b.rs\t2024-01-01\n+++ b.rs\n@@ -1 +1 @@\n-x\n+y",
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/a.py");
        let lines = &files[0].hunks[0].lines;
        assert_eq!((lines[0].old_no, lines[0].new_no), (Some(10), Some(10)));
        assert_eq!((lines[1].old_no, lines[1].new_no), (Some(11), None));
        assert_eq!((lines[2].old_no, lines[2].new_no), (None, Some(11)));
        assert_eq!(files[1].path, "b.rs");
        assert_eq!(files[1].hunks[0].lines.len(), 2);
    }

    #[test]
    fn new_files_and_lone_dashes() {
        let files =
            parse_fence_diff("--- /dev/null\n+++ b/new.ts\n@@ -0,0 +1 @@\n+a\n--- not a header");
        assert_eq!(files[0].status, crate::changes::FileStatus::Added);
        assert_eq!(files[0].path, "new.ts");
        // A `---` without its `+++` partner is a deleted line.
        let last = files[0].hunks[0].lines.last().unwrap();
        assert_eq!(
            (last.kind, last.text.as_str()),
            (LineKind::Del, "-- not a header")
        );
    }

    #[test]
    fn highlight_input_merges_code_lines_under_the_header_language() {
        let (lang, source) = highlight_input(
            Some("diff"),
            "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-let a;\n+let b;\n\\ meta",
        )
        .unwrap();
        assert_eq!(lang, LanguageId::Rust);
        assert_eq!(source, "let a;\nlet b;");
        assert!(highlight_input(Some("diff"), CHAT_DIFF).is_none());
        assert!(matches!(
            highlight_input(Some("rust"), "fn x() {}"),
            Some((LanguageId::Rust, Cow::Borrowed(_)))
        ));
    }

    #[test]
    fn find_counts_skip_markers_and_headers() {
        assert_eq!(count_matches("@@ x\n+x\n-x\n x", "x"), 3);
    }
}
