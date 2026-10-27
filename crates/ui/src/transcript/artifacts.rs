//! The replies' named artifacts, newest revision per title. The model
//! revises a page by writing it again under the same title, so every card
//! with that title opens the newest version, and a revision landing while
//! the page's tab is open reloads it ([`crate::markdown::artifact::ArtifactRevised`]).

use std::collections::HashMap;

use gpui::SharedString;

use super::{Row, RowKind};
use crate::markdown::artifact::ArtifactOpen;
use crate::markdown::parser::{ArtifactKind, Block};

#[derive(Default)]
pub(super) struct ArtifactIndex {
    latest: HashMap<SharedString, ArtifactOpen>,
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

    /// Re-index `rows`, returning the named pages whose newest revision
    /// changed since the last index. A `quiet` index (a chat's replay)
    /// returns none: those pages were written before it was opened.
    pub(super) fn update(&mut self, rows: &[Row], quiet: bool) -> Vec<ArtifactOpen> {
        let mut found: HashMap<&str, (String, ArtifactKind, &str)> = HashMap::new();
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
            visit(&top.block, &mut |kind, title, code, closed| {
                if closed || settled {
                    found.insert(
                        title,
                        (format!("{}#{block_ix}.{nested}", row.id), kind, code),
                    );
                }
                nested += 1;
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
                code: code.to_string().into(),
            };
            if !quiet {
                revised.push(artifact.clone());
            }
            self.latest.insert(artifact.title.clone(), artifact);
        }
        revised
    }
}

/// Every named artifact in `block`, in document order.
fn visit<'a>(block: &'a Block, f: &mut impl FnMut(ArtifactKind, &'a str, &'a str, bool)) {
    match block {
        Block::Artifact {
            kind,
            title: Some(title),
            code,
            closed,
        } => f(*kind, title, code, *closed),
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
}
