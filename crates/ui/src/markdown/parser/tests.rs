use super::*;

fn stream(chunks: usize, text: &str) -> IncrementalParser {
    let mut p = IncrementalParser::default();
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = (start + chunks).min(bytes.len());
        while end < bytes.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        p.append(&text[start..end]);
        start = end;
    }
    p
}

const CORPORA: &[&str] = &[
    "# Title\n\nHello **bold** and *italic* and `code` and ~~gone~~.\n",
    "Paragraph one\nlazy continuation\n\nParagraph two with a [link](https://x.dev).\n",
    "- item one\n- item two\n  - nested a\n  - nested b\n- item three\n\ntail\n",
    "1. first\n2. second\n\n   loose paragraph in item\n\n3. third\n",
    "```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\nafter code\n",
    "intro\n\n```\nunclosed fence streaming",
    "> quoted line\n> more quote\n>\n> - a list in a quote\n\nplain\n",
    "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\ndone\n",
    "setext candidate\n===\n\nnext para\n---\n",
    "***\n\ntext between rules\n\n---\n",
    "- [x] done task\n- [ ] open task\n",
    "    indented code line one\n    line two\n\npara\n",
    "para with <span>inline html</span> inside\n\n<div>\nblock html\n</div>\n",
    "###### deep heading\n\n#### h4\n",
];

#[test]
fn incremental_matches_full_on_streamed_corpora() {
    for (ci, corpus) in CORPORA.iter().enumerate() {
        let full = parse_full(corpus);
        for chunk in [1usize, 2, 3, 7, 16, 64] {
            let inc = stream(chunk, corpus);
            assert_eq!(
                inc.tree(),
                &full,
                "corpus {ci} diverged at chunk size {chunk}:\n{corpus}"
            );
        }
    }
}

#[test]
fn appends_keep_committed_blocks_identical() {
    // Streaming stability invariant: blocks before the reparse boundary
    // (everything but the last two top-level blocks) must be reused
    // as-is across appends — same index, same value — so row/element keys
    // never re-mount and earlier blocks can never visibly reflow.
    for corpus in CORPORA {
        let mut p = IncrementalParser::default();
        let mut prev = p.tree().clone();
        let bytes = corpus.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            let mut end = (start + 3).min(bytes.len());
            while end < bytes.len() && !corpus.is_char_boundary(end) {
                end += 1;
            }
            p.append(&corpus[start..end]);
            start = end;

            let cur = p.tree();
            let committed = prev.blocks.len().saturating_sub(2);
            assert!(
                cur.blocks.len() >= committed,
                "committed blocks disappeared:\n{corpus}"
            );
            for i in 0..committed {
                assert_eq!(
                    cur.blocks[i], prev.blocks[i],
                    "block {i} changed across an append:\n{corpus}"
                );
            }
            prev = cur.clone();
        }
    }
}

#[test]
fn incremental_matches_full_with_link_definitions() {
    // Definitions act at a distance → parser falls back to full reparses,
    // so parity must still hold.
    let corpus = "See [docs] for more.\n\nMore text.\n\n[docs]: https://example.com\n";
    let full = parse_full(corpus);
    for chunk in [1usize, 3, 9] {
        assert_eq!(stream(chunk, corpus).tree(), &full, "chunk {chunk}");
    }
    // The reference actually resolved into a link.
    let has_link = full.blocks.iter().any(|b| match &b.block {
        Block::Paragraph { runs } => runs.iter().any(|r| r.style.link.is_some()),
        _ => false,
    });
    assert!(has_link, "expected [docs] to resolve to a link");
}

#[test]
fn set_text_appends_or_resets() {
    let mut p = IncrementalParser::default();
    p.set_text("hello");
    p.set_text("hello world");
    assert_eq!(p.tree(), &parse_full("hello world"));
    // Non-append rewrites reset cleanly.
    p.set_text("different");
    assert_eq!(p.tree(), &parse_full("different"));
    assert_eq!(p.source(), "different");
}

#[test]
fn block_structure_basics() {
    let tree = parse_full("## Head\n\npara **b _bi_** text\n\n```ts\nlet x = 1;\n```\n");
    assert_eq!(tree.len(), 3);
    match &tree.blocks[0].block {
        Block::Heading { level, runs } => {
            assert_eq!(*level, 2);
            assert_eq!(runs[0].text, "Head");
        }
        other => panic!("unexpected {other:?}"),
    }
    match &tree.blocks[1].block {
        Block::Paragraph { runs } => {
            assert_eq!(runs.len(), 4); // "para ", "b ", "bi" (bold+italic), " text"
            assert!(runs[1].style.bold && !runs[1].style.italic);
            assert!(runs[2].style.bold && runs[2].style.italic);
        }
        other => panic!("unexpected {other:?}"),
    }
    match &tree.blocks[2].block {
        Block::CodeBlock { language, code } => {
            assert_eq!(language.as_deref(), Some("ts"));
            assert_eq!(code, "let x = 1;");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn nested_lists_and_tight_items() {
    let tree = parse_full("- a\n  - a1\n  - a2\n- b\n");
    let Block::List {
        ordered_start,
        items,
    } = &tree.blocks[0].block
    else {
        panic!("expected list");
    };
    assert_eq!(*ordered_start, None);
    assert_eq!(items.len(), 2);
    // Tight item text became an implicit paragraph, nested list follows.
    assert!(matches!(items[0][0], Block::Paragraph { .. }));
    assert!(matches!(items[0][1], Block::List { .. }));
}

#[test]
fn tables_parse_header_and_rows() {
    let tree = parse_full("| a | b |\n|---|---|\n| 1 | 2 |\n");
    let Block::Table {
        header,
        rows,
        align,
    } = &tree.blocks[0].block
    else {
        panic!("expected table");
    };
    assert_eq!(header.len(), 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1][0].text, "2");
    assert_eq!(align, &vec![TableAlign::Left, TableAlign::Left]);
}

#[test]
fn tables_parse_column_alignment() {
    let tree = parse_full("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n");
    let Block::Table { align, .. } = &tree.blocks[0].block else {
        panic!("expected table");
    };
    assert_eq!(
        align,
        &vec![TableAlign::Left, TableAlign::Center, TableAlign::Right]
    );
}

#[test]
fn links_carry_urls() {
    let tree = parse_full("go to [zed](https://zed.dev) now\n");
    let Block::Paragraph { runs } = &tree.blocks[0].block else {
        panic!()
    };
    let link = runs
        .iter()
        .find(|r| r.style.link.is_some())
        .expect("link run");
    assert_eq!(link.text, "zed");
    assert_eq!(link.style.link.as_deref(), Some("https://zed.dev"));
}

/// The paragraph's single link run: (text, url).
fn only_link(source: &str) -> Option<(String, String)> {
    let tree = parse_full(source);
    let Block::Paragraph { runs } = &tree.blocks[0].block else {
        panic!()
    };
    let links: Vec<_> = runs
        .iter()
        .filter_map(|r| Some((r.text.clone(), r.style.link.clone()?)))
        .collect();
    assert!(links.len() <= 1, "expected at most one link: {links:?}");
    links.into_iter().next()
}

/// Bare URLs autolink (the GFM extension pulldown-cmark lacks): the URL
/// becomes a clickable run, trailing sentence punctuation stays text.
#[test]
fn bare_urls_autolink() {
    assert_eq!(
        only_link("PR is updated: https://github.com/zeronsh/comet/pull/31\n"),
        Some((
            "https://github.com/zeronsh/comet/pull/31".into(),
            "https://github.com/zeronsh/comet/pull/31".into()
        ))
    );
    assert_eq!(
        only_link("see https://x.dev/a, then rest.\n").map(|l| l.1),
        Some("https://x.dev/a".into())
    );
    // A wrapping paren is shed; one balanced by an opener in the path stays.
    assert_eq!(
        only_link("(docs: https://x.dev/Foo_(bar))\n").map(|l| l.1),
        Some("https://x.dev/Foo_(bar)".into())
    );
    // Bold text still autolinks, and the run keeps the emphasis.
    let tree = parse_full("**see https://x.dev now**\n");
    let Block::Paragraph { runs } = &tree.blocks[0].block else {
        panic!()
    };
    let link = runs.iter().find(|r| r.style.link.is_some()).unwrap();
    assert!(link.style.bold);
    assert_eq!(link.style.link.as_deref(), Some("https://x.dev"));
}

/// Non-links stay text: glued schemes, bare schemes, code spans, and the
/// destination text of a real markdown link.
#[test]
fn autolink_leaves_non_urls_alone() {
    assert_eq!(only_link("foohttps://x.dev is glued\n"), None);
    assert_eq!(only_link("the https:// scheme alone\n"), None);
    assert_eq!(only_link("`https://x.dev` in code\n"), None);
    // A markdown link whose TEXT is a URL keeps the written destination.
    assert_eq!(
        only_link("[https://shown.dev](https://real.dev)\n"),
        Some(("https://shown.dev".into(), "https://real.dev".into()))
    );
}

#[test]
fn top_level_ranges_are_stable_anchors() {
    let src = "first\n\nsecond\n\nthird";
    let tree = parse_full(src);
    assert_eq!(tree.len(), 3);
    assert!(
        tree.blocks
            .windows(2)
            .all(|w| w[0].range.start < w[1].range.start)
    );
    assert_eq!(&src[tree.blocks[1].range.clone()], "second\n");
}

/// Concatenated visible text of a block (what the user reads).
fn flat(block: &Block) -> String {
    fn walk(b: &Block, out: &mut String) {
        match b {
            Block::Paragraph { runs } | Block::Heading { runs, .. } => {
                for r in runs {
                    out.push_str(&r.text);
                }
            }
            Block::BlockQuote { children } => children.iter().for_each(|c| walk(c, out)),
            Block::List { items, .. } => items.iter().flatten().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let mut s = String::new();
    walk(block, &mut s);
    s
}

#[test]
fn display_tree_styles_hanging_bold_immediately() {
    let mut p = IncrementalParser::default();
    p.set_text("intro **bo");
    let display = p.display_tree();
    let Block::Paragraph { runs } = &display.blocks[0].block else {
        panic!("expected paragraph");
    };
    let bold: Vec<_> = runs.iter().filter(|r| r.style.bold).collect();
    assert_eq!(bold.len(), 1);
    assert_eq!(bold[0].text, "bo");
    assert!(!flat(&display.blocks[0].block).contains("**"));
    // The canonical tree stays honest: literal markers until truly closed.
    assert!(flat(&p.tree().blocks[0].block).contains("**"));
}

#[test]
fn display_tree_converges_to_canonical_when_balanced() {
    let corpus = "a **b** *c* `d` [e](https://x.dev) ~~f~~";
    let mut p = IncrementalParser::default();
    p.set_text(corpus);
    assert_eq!(p.display_tree(), *p.tree());
    assert_eq!(p.tree(), &parse_full(corpus));
}

#[test]
fn display_tree_never_leaks_streaming_urls() {
    let full = "read [docs](https://example.com/long/path) now";
    let mut p = IncrementalParser::default();
    for i in 1..=full.len() {
        if !full.is_char_boundary(i) {
            continue;
        }
        p.set_text(&full[..i]);
        let text = flat(&p.display_tree().blocks[0].block);
        assert!(!text.contains("http"), "url leaked at {i}: {text:?}");
    }
    // Mid-URL the link text carries the pending sentinel destination.
    let mut p = IncrementalParser::default();
    p.set_text("read [docs](https://exa");
    let Block::Paragraph { runs } = &p.display_tree().blocks[0].block else {
        panic!("expected paragraph");
    };
    let link = runs
        .iter()
        .find(|r| r.style.link.is_some())
        .expect("link run");
    assert_eq!(link.text, "docs");
    assert_eq!(
        link.style.link.as_deref(),
        Some(crate::markdown::mend::PENDING_LINK_URL)
    );
}

#[test]
fn display_tree_leaves_code_blocks_alone() {
    let mut p = IncrementalParser::default();
    p.set_text("intro\n\n```\nunclosed **fence");
    assert_eq!(p.display_tree(), *p.tree());
}

#[test]
fn display_tree_suppresses_setext_flicker() {
    // "para" + "\n-" parses as an H2 for exactly one chunk before the
    // list item's text arrives; the display tree keeps it a paragraph.
    let mut p = IncrementalParser::default();
    p.set_text("para\n-");
    let display = p.display_tree();
    assert!(
        matches!(
            display.blocks.last().unwrap().block,
            Block::Paragraph { .. }
        ),
        "expected paragraph, got {display:?}"
    );
}

#[test]
fn display_tree_prefix_matches_canonical_across_streams() {
    // Mending swaps only the last block: everything before it must be
    // byte-identical to the canonical tree so render caches and row keys
    // survive.
    for corpus in CORPORA {
        let mut p = IncrementalParser::default();
        let bytes = corpus.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            let mut end = (start + 3).min(bytes.len());
            while end < bytes.len() && !corpus.is_char_boundary(end) {
                end += 1;
            }
            p.append(&corpus[start..end]);
            start = end;

            let display = p.display_tree();
            let canonical = p.tree();
            for i in 0..canonical.blocks.len().saturating_sub(1) {
                assert_eq!(
                    display.blocks[i], canonical.blocks[i],
                    "display prefix diverged:\n{corpus}"
                );
            }
        }
    }
}

#[test]
fn empty_and_whitespace_sources() {
    assert!(parse_full("").is_empty());
    assert!(parse_full("\n\n  \n").is_empty());
    let mut p = IncrementalParser::default();
    p.append("");
    assert!(p.tree().is_empty());
}
