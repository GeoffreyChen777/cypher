//! The replies' named artifacts, newest revision per title. The model
//! revises a page by writing it again under the same title, so every card
//! with that title opens the newest version, and a revision landing while
//! the page's tab is open reloads it ([`crate::markdown::artifact::ArtifactRevised`]).
//! A patch fence is a revision too: its hunks applied, in chat order, to the
//! page's version before it.

use std::collections::HashMap;

use gpui::SharedString;

use super::{Row, RowKind};
use crate::markdown::artifact::{ArtifactOpen, PatchOutcome, patch};
use crate::markdown::parser::{ArtifactKind, Block};

#[derive(Default)]
pub(super) struct ArtifactIndex {
    latest: HashMap<SharedString, ArtifactOpen>,
    /// Each row's patch fences by their code, and how they went.
    patches: HashMap<SharedString, Vec<(String, PatchOutcome)>>,
}

impl ArtifactIndex {
    /// What a click on `artifact` opens: a named page's newest revision.
    pub(super) fn latest(&self, artifact: ArtifactOpen) -> ArtifactOpen {
        if !artifact.named {
            return artifact;
        }
        self.latest
            .get(&artifact.title)
            .cloned()
            .unwrap_or(artifact)
    }

    /// How the patch fence with `code` in row `row` went.
    pub(super) fn patch_outcome(&self, row: &str, code: &str) -> Option<PatchOutcome> {
        self.patches
            .get(row)?
            .iter()
            .find(|(patch, _)| patch == code)
            .map(|(_, outcome)| *outcome)
    }

    /// Re-index `rows`, returning the named pages whose newest revision
    /// changed since the last index. A `quiet` index (a chat's replay)
    /// returns none: those pages were written before it was opened.
    pub(super) fn update(&mut self, rows: &[Row], quiet: bool) -> Vec<ArtifactOpen> {
        let mut found: HashMap<&str, (String, ArtifactKind, String)> = HashMap::new();
        self.patches.clear();
        for row in rows {
            let (RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix }) =
                &row.kind
            else {
                continue;
            };
            let Some(top) = tree.blocks.get(*block_ix) else {
                continue;
            };
            // A streaming fence isn't a revision yet; a settled reply's
            // unclosed one opens like its card does.
            let settled = matches!(row.kind, RowKind::Markdown { .. });
            let mut nested = 0;
            visit(&top.block, &mut |kind, title, code, closed, is_patch| {
                let key = format!("{}#{block_ix}.{nested}", row.id);
                nested += 1;
                if !(closed || settled) {
                    return;
                }
                if !is_patch {
                    found.insert(title, (key, kind, code.to_string()));
                    return;
                }
                let base = found
                    .get(title)
                    .map(|(_, kind, base)| (*kind, patch::apply(base, code)));
                let outcome = match base {
                    Some((kind, applied)) => {
                        let outcome = PatchOutcome {
                            applied: applied.applied,
                            total: applied.total,
                            found_page: true,
                        };
                        // An update with nothing applied leaves the page
                        // where it was.
                        if applied.applied > 0 {
                            found.insert(title, (key, kind, applied.code));
                        }
                        outcome
                    }
                    None => PatchOutcome {
                        applied: 0,
                        total: patch::hunks(code).len(),
                        found_page: false,
                    },
                };
                self.patches
                    .entry(row.id.clone())
                    .or_default()
                    .push((code.to_string(), outcome));
            });
        }
        let mut revised = Vec::new();
        self.latest
            .retain(|title, _| found.contains_key(title.as_ref()));
        for (title, (key, kind, code)) in found {
            if self
                .latest
                .get(title)
                .is_some_and(|shown| shown.key.as_ref() == key)
            {
                continue;
            }
            let artifact = ArtifactOpen {
                key: key.into(),
                kind,
                title: title.to_string().into(),
                named: true,
                code: code.into(),
            };
            if !quiet {
                revised.push(artifact.clone());
            }
            self.latest.insert(artifact.title.clone(), artifact);
        }
        revised
    }
}

/// Every named artifact in `block`, patches included, in document order.
fn visit<'a>(block: &'a Block, f: &mut impl FnMut(ArtifactKind, &'a str, &'a str, bool, bool)) {
    match block {
        Block::Artifact {
            kind,
            title: Some(title),
            code,
            closed,
            patch,
        } => f(*kind, title, code, *closed, *patch),
        Block::BlockQuote { children } => children.iter().for_each(|child| visit(child, f)),
        Block::List { items, .. } => items.iter().flatten().for_each(|child| visit(child, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cypher_doc::MessageRole;

    use super::*;
    use crate::markdown::parser::parse_full;

    fn rows(replies: &[(&str, &str, bool)]) -> Vec<Row> {
        replies
            .iter()
            .flat_map(|(id, text, live)| {
                let tree = Arc::new(parse_full(text));
                (0..tree.len()).map(move |block_ix| {
                    let tree = tree.clone();
                    Row {
                        id: format!("{id}.{block_ix}").into(),
                        version: 0,
                        turn_start: block_ix == 0,
                        kind: if *live {
                            RowKind::LiveMarkdown { tree, block_ix }
                        } else {
                            RowKind::Markdown { tree, block_ix }
                        },
                        entry_id: id.to_string().into(),
                        role: MessageRole::Assistant,
                        timestamp: None,
                        answered: None,
                        translation: None,
                    }
                })
            })
            .collect()
    }

    const FIRST: &str = "```html artifact title=\"Demo\"\n<p>one</p>\n```\n";
    const SECOND: &str = "Revised:\n\n```html artifact title=\"Demo\"\n<p>two</p>\n```\n";

    fn opened(index: &ArtifactIndex, code: &str) -> SharedString {
        index
            .latest(ArtifactOpen {
                key: "card".into(),
                kind: ArtifactKind::Html,
                title: "Demo".into(),
                named: true,
                code: code.to_string().into(),
            })
            .code
    }

    #[test]
    fn every_card_of_a_named_page_opens_its_newest_revision() {
        let mut index = ArtifactIndex::default();
        assert!(
            index
                .update(&rows(&[("a", FIRST, false), ("b", SECOND, false)]), true)
                .is_empty()
        );
        assert_eq!(opened(&index, "<p>one</p>"), "<p>two</p>");
        let unnamed = ArtifactOpen {
            key: "card".into(),
            kind: ArtifactKind::Html,
            title: "Demo".into(),
            named: false,
            code: "<p>mine</p>".into(),
        };
        assert_eq!(index.latest(unnamed.clone()), unnamed);
    }

    #[test]
    fn a_revision_is_reported_once_its_fence_closes() {
        let mut index = ArtifactIndex::default();
        index.update(&rows(&[("a", FIRST, false)]), true);
        let streaming = "```html artifact title=\"Demo\"\n<p>tw";
        assert!(
            index
                .update(&rows(&[("a", FIRST, false), ("b", streaming, true)]), false)
                .is_empty()
        );
        let revised = index.update(&rows(&[("a", FIRST, false), ("b", SECOND, true)]), false);
        assert_eq!(revised.len(), 1);
        assert_eq!(revised[0].code, "<p>two</p>");
        // The reply settling changes nothing about the page.
        assert!(
            index
                .update(&rows(&[("a", FIRST, false), ("b", SECOND, false)]), false)
                .is_empty()
        );
        // A new title is a page of its own; the old one keeps its revision.
        let other = "```html artifact title=\"Other\"\n<p>x</p>\n```\n";
        let revised = index.update(
            &rows(&[
                ("a", FIRST, false),
                ("b", SECOND, false),
                ("c", other, false),
            ]),
            false,
        );
        assert_eq!(revised.len(), 1);
        assert_eq!(revised[0].title, "Other");
        assert_eq!(opened(&index, ""), "<p>two</p>");
    }

    const PATCH: &str = "```html artifact-patch title=\"Demo\"\n<<<<<<< SEARCH\n<p>one</p>\n=======\n<p>one, patched</p>\n>>>>>>> REPLACE\n<<<<<<< SEARCH\n<p>gone</p>\n=======\nx\n>>>>>>> REPLACE\n```\n";

    fn patch_code() -> String {
        match &parse_full(PATCH).blocks[0].block {
            Block::Artifact { code, .. } => code.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_patch_revises_the_page_before_it_and_reports_how_it_went() {
        let mut index = ArtifactIndex::default();
        index.update(&rows(&[("a", FIRST, false)]), true);
        let revised = index.update(&rows(&[("a", FIRST, false), ("b", PATCH, true)]), false);
        assert_eq!(revised.len(), 1);
        assert_eq!(revised[0].code, "<p>one, patched</p>");
        assert_eq!(opened(&index, ""), "<p>one, patched</p>");
        assert_eq!(
            index.patch_outcome("b.0", &patch_code()),
            Some(PatchOutcome {
                applied: 1,
                total: 2,
                found_page: true
            })
        );
        // A full page after it replaces the patched one.
        index.update(
            &rows(&[
                ("a", FIRST, false),
                ("b", PATCH, false),
                ("c", SECOND, false),
            ]),
            false,
        );
        assert_eq!(opened(&index, ""), "<p>two</p>");
    }

    #[test]
    fn a_patch_with_no_page_before_it_opens_nothing() {
        let mut index = ArtifactIndex::default();
        assert!(
            index
                .update(&rows(&[("b", PATCH, false)]), false)
                .is_empty()
        );
        assert_eq!(
            index.patch_outcome("b.0", &patch_code()),
            Some(PatchOutcome {
                applied: 0,
                total: 2,
                found_page: false
            })
        );
        assert_eq!(opened(&index, "<p>card</p>"), "<p>card</p>");
    }
}
