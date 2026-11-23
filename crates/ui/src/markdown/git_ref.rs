//! Pull request, issue and commit references in replies: `#123`,
//! `owner/repo#123`, a commit SHA, a GitHub URL. Detection is the shared
//! [`cypher_proto::find_git_refs`]; this module turns its matches into link
//! runs of their own scheme ([`ref_link`]) so the renderer lays them out
//! like any link (in neutral ink), and reports the pointer on one to the
//! surface, which shows the hover card ([`crate::transcript`]).
//!
//! Only a chat with a git checkout detects anything (the surface wires
//! [`GitRefUi`] only then): a bare `#12` or SHA means nothing elsewhere.
//! Fenced code is never scanned (it is no inline runs); a link keeps its
//! own target, except that a GitHub pull request / issue / commit URL
//! still gets the card ([`ref_of_link`]).

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use cypher_proto::GitRef;
use cypher_proto::git_refs::{GitRefMatch, find_git_refs};
use gpui::{Bounds, SharedString, Window, canvas, prelude::*};

use super::parser::InlineRun;
use super::selection::SelectionScope;

/// The scheme of a detected reference's link run. Never leaves the app:
/// clicks on it go to [`GitRefUi::on_click`].
const SCHEME: &str = "cypher-gitref:";

/// The link a detected reference becomes.
pub fn ref_link(git_ref: &GitRef) -> String {
    match git_ref {
        GitRef::Issue { repo, number } => {
            format!("{SCHEME}issue:{}:{number}", repo.as_deref().unwrap_or(""))
        }
        GitRef::Commit { repo, sha } => {
            format!("{SCHEME}commit:{}:{sha}", repo.as_deref().unwrap_or(""))
        }
    }
}

/// Whether `url` is a detected reference's own link ([`ref_link`]).
pub fn is_ref_link(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// The reference a link stands for: a detected one ([`ref_link`]), or a
/// GitHub pull request, issue or commit URL the reply linked itself.
pub fn ref_of_link(url: &str) -> Option<GitRef> {
    if let Some(rest) = url.strip_prefix(SCHEME) {
        let mut parts = rest.splitn(3, ':');
        let (kind, repo, id) = (parts.next()?, parts.next()?, parts.next()?);
        let repo = (!repo.is_empty()).then(|| repo.to_owned());
        let git_ref = match kind {
            "issue" => GitRef::Issue {
                repo,
                number: id.parse().ok()?,
            },
            "commit" => GitRef::Commit {
                repo,
                sha: id.to_owned(),
            },
            _ => return None,
        };
        return git_ref.is_valid().then_some(git_ref);
    }
    // A URL is all-or-nothing in code mode: the whole destination must be
    // one GitHub ref URL.
    let found = find_git_refs(url, true);
    match found.as_slice() {
        [one] if one.range == (0..url.len()) && one.git_ref.repo().is_some() => {
            Some(one.git_ref.clone())
        }
        _ => None,
    }
}

/// `runs` with every reference in prose and inline code made a link run
/// ([`ref_link`]); `None` when there is none (the runs render as they are).
///
/// Consecutive prose runs (plain, bold, italic — not code, links or
/// formulas) are scanned as one text, so emphasis boundaries don't hide
/// what is before a `#` (`C**#**1` is no ref) and a ref split across them
/// still matches; the pieces keep their own styles. An inline code span
/// is one reference or nothing, and keeps its code look.
pub fn linkify(runs: &[InlineRun]) -> Option<Vec<InlineRun>> {
    let is_prose =
        |run: &InlineRun| !run.style.code && run.style.link.is_none() && run.style.math.is_none();
    let mut out: Vec<InlineRun> = Vec::with_capacity(runs.len());
    let mut found_any = false;
    let mut ix = 0;
    while ix < runs.len() {
        let run = &runs[ix];
        if run.style.code && run.style.link.is_none() && run.style.math.is_none() {
            match find_git_refs(&run.text, true).first() {
                Some(found) => {
                    found_any = true;
                    let mut linked = run.clone();
                    linked.style.link = Some(ref_link(&found.git_ref));
                    out.push(linked);
                }
                None => out.push(run.clone()),
            }
            ix += 1;
            continue;
        }
        if !is_prose(run) {
            out.push(run.clone());
            ix += 1;
            continue;
        }
        let end = runs[ix..]
            .iter()
            .position(|run| !is_prose(run))
            .map_or(runs.len(), |len| ix + len);
        let group = &runs[ix..end];
        let text: String = group.iter().map(|run| run.text.as_str()).collect();
        let found = find_git_refs(&text, false);
        if found.is_empty() {
            out.extend(group.iter().cloned());
        } else {
            found_any = true;
            split_group(&mut out, group, &found);
        }
        ix = end;
    }
    found_any.then_some(out)
}

/// Push `group`'s runs cut at every match boundary, the pieces inside a
/// match linked to it.
fn split_group(out: &mut Vec<InlineRun>, group: &[InlineRun], found: &[GitRefMatch]) {
    let mut at = 0;
    for run in group {
        let span = at..at + run.text.len();
        let mut cuts: Vec<usize> = vec![span.start, span.end];
        for m in found {
            for edge in [m.range.start, m.range.end] {
                if edge > span.start && edge < span.end {
                    cuts.push(edge);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            let mut piece = InlineRun {
                text: run.text[start - span.start..end - span.start].to_owned(),
                style: run.style.clone(),
            };
            if let Some(m) = found
                .iter()
                .find(|m| m.range.start <= start && end <= m.range.end)
            {
                piece.style.link = Some(ref_link(&m.git_ref));
            }
            out.push(piece);
        }
        at = span.end;
    }
}

/// A reference under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct GitRefHover {
    /// Its identity: its text element's key and its index there.
    pub key: SharedString,
    /// The row it sits in.
    pub row_key: SharedString,
    pub git_ref: GitRef,
    /// Its first visual line, in window coordinates.
    pub anchor: Bounds<gpui::Pixels>,
}

/// The pointer entered a reference (`Some`) or left the one it was on.
pub type GitRefHoverHandler = Rc<dyn Fn(Option<GitRefHover>, &mut Window, &mut gpui::App)>;
/// A reference was clicked.
pub type GitRefClickHandler = Rc<dyn Fn(GitRefHover, &mut Window, &mut gpui::App)>;

/// What a surface does with references; wired only for a chat with a git
/// checkout.
#[derive(Clone)]
pub struct GitRefUi {
    pub on_hover: GitRefHoverHandler,
    pub on_click: GitRefClickHandler,
}

/// A text element's references: the link ranges that are one.
pub fn spans(links: &[(Range<usize>, String)]) -> Vec<(Range<usize>, GitRef)> {
    links
        .iter()
        .filter_map(|(range, url)| ref_of_link(url).map(|git_ref| (range.clone(), git_ref)))
        .collect()
}

/// The hover report of `span`, the `ix`-th reference of element `key`.
pub fn hover_for(
    key: &str,
    row_key: &SharedString,
    ix: usize,
    span: &(Range<usize>, GitRef),
    layout: &gpui::TextLayout,
) -> GitRefHover {
    let anchor = super::render::range_rects(layout, &span.0, 0.0, 0.0)
        .into_iter()
        .next()
        .unwrap_or_default();
    GitRefHover {
        key: format!("{key}@git{ix}").into(),
        row_key: row_key.clone(),
        git_ref: span.1.clone(),
        anchor,
    }
}

thread_local! {
    /// The reference under the pointer: `(scope, element key, index)`.
    static HOVERED: RefCell<Option<(SelectionScope, Arc<str>, usize)>> = const { RefCell::new(None) };
}

/// Forget the hovered reference of `scope` (its card closed, or it
/// scrolled away under a still pointer).
pub fn clear_hover(scope: SelectionScope) {
    HOVERED.with(|hovered| {
        let mut hovered = hovered.borrow_mut();
        if hovered.as_ref().is_some_and(|(s, _, _)| *s == scope) {
            *hovered = None;
        }
    });
}

/// An invisible layer over a text element's references that reports which
/// one the pointer is on (hitboxes, so a popover over the text wins). A
/// drag never hovers: it is selecting text.
pub fn hover_overlay(
    ui: GitRefUi,
    scope: SelectionScope,
    key: Arc<str>,
    row_key: SharedString,
    spans: Vec<(Range<usize>, GitRef)>,
    layout: gpui::TextLayout,
) -> impl IntoElement {
    let (hit_layout, hit_spans) = (layout.clone(), spans.clone());
    canvas(
        move |_, window, _| {
            hit_spans
                .iter()
                .map(|(range, _)| {
                    super::render::range_rects(&hit_layout, range, 0.0, 0.0)
                        .into_iter()
                        .map(|rect| window.insert_hitbox(rect, gpui::HitboxBehavior::Normal))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        },
        move |_, hitboxes, window, _| {
            window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                let over = if e.dragging() {
                    None
                } else {
                    hitboxes
                        .iter()
                        .position(|lines| lines.iter().any(|hitbox| hitbox.is_hovered(window)))
                };
                let current = HOVERED.with(|hovered| match &*hovered.borrow() {
                    Some((s, k, ix)) if *s == scope && k.as_ref() == key.as_ref() => Some(*ix),
                    _ => None,
                });
                if over == current {
                    return;
                }
                match over {
                    Some(ix) => {
                        HOVERED.with(|hovered| *hovered.borrow_mut() = Some((scope, key.clone(), ix)));
                        let hover = hover_for(&key, &row_key, ix, &spans[ix], &layout);
                        (ui.on_hover)(Some(hover), window, cx);
                    }
                    // Only the element that held the hover gives it up.
                    None => {
                        HOVERED.with(|hovered| *hovered.borrow_mut() = None);
                        (ui.on_hover)(None, window, cx);
                    }
                }
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parser::{InlineStyle, parse_full as parse};

    fn plain(text: &str) -> InlineRun {
        InlineRun {
            text: text.into(),
            style: InlineStyle::default(),
        }
    }

    /// `(text, link)` of every run of the first paragraph of `md`, linked.
    fn linked(md: &str) -> Vec<(String, Option<String>)> {
        let tree = parse(md);
        let runs = match &tree.blocks[0].block {
            crate::markdown::parser::Block::Paragraph { runs } => runs.clone(),
            other => panic!("not a paragraph: {other:?}"),
        };
        linkify(&runs)
            .unwrap_or(runs)
            .into_iter()
            .map(|run| (run.text, run.style.link))
            .collect()
    }

    fn links(md: &str) -> Vec<(String, GitRef)> {
        let tree = parse(md);
        let runs = match &tree.blocks[0].block {
            crate::markdown::parser::Block::Paragraph { runs } => runs.clone(),
            _ => return Vec::new(),
        };
        let runs = linkify(&runs).unwrap_or(runs);
        let mut out: Vec<(String, GitRef)> = Vec::new();
        for run in runs {
            let Some(git_ref) = run.style.link.as_deref().and_then(ref_of_link) else {
                continue;
            };
            match out.last_mut() {
                Some((text, last)) if *last == git_ref => text.push_str(&run.text),
                _ => out.push((run.text, git_ref)),
            }
        }
        out
    }

    fn issue(repo: Option<&str>, number: u64) -> GitRef {
        GitRef::Issue {
            repo: repo.map(str::to_owned),
            number,
        }
    }

    fn commit(sha: &str) -> GitRef {
        GitRef::Commit {
            repo: None,
            sha: sha.into(),
        }
    }

    #[test]
    fn prose_refs_become_their_own_links() {
        assert_eq!(
            links("Fixed in #12 and cypher-ai/cypher#7, see a1b2c3d."),
            vec![
                ("#12".into(), issue(None, 12)),
                ("cypher-ai/cypher#7".into(), issue(Some("cypher-ai/cypher"), 7)),
                ("a1b2c3d".into(), commit("a1b2c3d")),
            ]
        );
        // The prose around them is untouched.
        let runs = linked("Fixed in #12.");
        assert_eq!(
            runs.iter().map(|(text, _)| text.as_str()).collect::<String>(),
            "Fixed in #12."
        );
        assert_eq!(runs[0].1, None);
        assert_eq!(runs[2], (".".into(), None));
    }

    #[test]
    fn inline_code_is_one_ref_or_nothing() {
        assert_eq!(links("Commit `deadbeef` landed"), vec![("deadbeef".into(), commit("deadbeef"))]);
        assert_eq!(links("Run `git show a1b2c3d` now"), vec![]);
        assert_eq!(links("See `#42`."), vec![("#42".into(), issue(None, 42))]);
        // The code span keeps its code look.
        let tree = parse("Commit `deadbeef` landed");
        let crate::markdown::parser::Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!()
        };
        let runs = linkify(runs).unwrap();
        assert!(runs.iter().any(|run| run.style.code && run.style.link.is_some()));
    }

    #[test]
    fn cjk_neighbours_still_match() {
        assert_eq!(
            links("已在#12修复，提交a1b2c3d。"),
            vec![("#12".into(), issue(None, 12))]
        );
        assert_eq!(
            links("提交 a1b2c3d 已合并"),
            vec![("a1b2c3d".into(), commit("a1b2c3d"))]
        );
    }

    #[test]
    fn emphasis_is_scanned_as_one_text() {
        // A bold ref is still a ref, its piece bold and linked.
        let tree = parse("Merged **#12** today");
        let crate::markdown::parser::Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!()
        };
        let runs = linkify(runs).unwrap();
        let piece = runs.iter().find(|run| run.text == "#12").unwrap();
        assert!(piece.style.bold && piece.style.link.is_some());
        // What precedes a `#` across a style boundary still counts: `C#1`.
        assert_eq!(links("C**#1**"), vec![]);
    }

    #[test]
    fn non_refs_stay_text() {
        assert_eq!(links("C# and F#7 and &#123; and 1234567 and deadbeef"), vec![]);
        assert!(linkify(&[plain("nothing to see")]).is_none());
    }

    #[test]
    fn links_keep_their_target_but_github_urls_get_the_card() {
        // A link elsewhere stays that link (its `#1` is no ref).
        assert_eq!(links("[the docs](https://example.com/a#1)"), vec![]);
        assert_eq!(links("[see #12](https://example.com)"), vec![]);
        // A GitHub pull request or commit URL, linked or bare, is the ref.
        assert_eq!(
            links("[the fix](https://github.com/o/r/pull/9)"),
            vec![("the fix".into(), issue(Some("o/r"), 9))]
        );
        assert_eq!(
            links("Landed: https://github.com/o/r/commit/a1b2c3d4e5"),
            vec![(
                "https://github.com/o/r/commit/a1b2c3d4e5".into(),
                GitRef::Commit {
                    repo: Some("o/r".into()),
                    sha: "a1b2c3d4e5".into()
                }
            )]
        );
    }

    #[test]
    fn fenced_code_is_never_scanned() {
        // A fence is a code block, not inline runs: nothing to linkify.
        let tree = parse("```\ngit revert a1b2c3d # fixes #12\n```");
        assert!(matches!(
            tree.blocks[0].block,
            crate::markdown::parser::Block::CodeBlock { .. }
        ));
        assert_eq!(links("```\n#12\n```"), vec![]);
    }

    #[test]
    fn ref_links_round_trip() {
        for git_ref in [
            issue(None, 12),
            issue(Some("o/r"), 3),
            commit("a1b2c3d"),
            GitRef::Commit {
                repo: Some("o/r".into()),
                sha: "a1b2c3d4".into(),
            },
        ] {
            let link = ref_link(&git_ref);
            assert!(is_ref_link(&link));
            assert_eq!(ref_of_link(&link), Some(git_ref));
        }
        assert_eq!(ref_of_link("cypher-gitref:issue::0"), None);
        assert_eq!(ref_of_link("https://github.com/o/r"), None);
        assert_eq!(ref_of_link("https://example.com/pull/1"), None);
    }
}
