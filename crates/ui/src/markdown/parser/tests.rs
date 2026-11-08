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
    "Euler: $e^{i\\pi} + 1 = 0$ holds.\n\n$$\n\\int_0^1 x\\,dx = \\tfrac12\n$$\n\nafter $a_1$ and $b_2$\n",
    "TeX delimiters \\(x_1\\) and\n\\[\n\\sum_k k\n\\]\nthen prose, prices $5 and $10\n",
    "Here it is:\n\n```html artifact title=\"Bubble sort\"\n<!doctype html>\n<p>hi</p>\n```\n\nafter\n",
    "- item\n\n  ````svg artifact\n  <svg/>\n  ````\n",
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
        Block::CodeBlock {
            language,
            code,
            closed,
        } => {
            assert_eq!(language.as_deref(), Some("ts"));
            assert_eq!(code, "let x = 1;");
            assert!(closed);
        }
        other => panic!("unexpected {other:?}"),
    }
}

fn closed_flags(source: &str) -> Vec<bool> {
    parse_full(source)
        .blocks
        .iter()
        .filter_map(|top| match &top.block {
            Block::CodeBlock { closed, .. } => Some(*closed),
            Block::List { items, .. } => items.iter().flatten().find_map(|block| match block {
                Block::CodeBlock { closed, .. } => Some(*closed),
                _ => None,
            }),
            Block::BlockQuote { children } => children.iter().find_map(|block| match block {
                Block::CodeBlock { closed, .. } => Some(*closed),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

#[test]
fn code_blocks_know_whether_their_fence_closed() {
    assert_eq!(
        closed_flags("```mermaid\ngraph TD\n  A --> B\n```\n"),
        [true]
    );
    assert_eq!(closed_flags("```mermaid\ngraph TD\n  A --> B\n```"), [true]);
    assert_eq!(closed_flags("```mermaid\ngraph TD\n  A --> B\n"), [false]);
    assert_eq!(closed_flags("```mermaid\ngraph TD\n  A --> B\n``"), [false]);
    assert_eq!(closed_flags("```mermaid"), [false]);
    // A shorter run inside a longer fence is content, not a closer.
    assert_eq!(closed_flags("````md\n```\nstill code\n```\n"), [false]);
    assert_eq!(closed_flags("````md\n```\ncode\n````\n"), [true]);
    assert_eq!(closed_flags("~~~\nx\n~~~\n"), [true]);
    assert_eq!(closed_flags("    indented\n    code\n"), [true]);
    // Container markers ride the block's lines.
    assert_eq!(closed_flags("- ```mermaid\n  graph TD\n  ```\n"), [true]);
    assert_eq!(closed_flags("> ```mermaid\n> graph TD\n> ```\n"), [true]);
    assert_eq!(closed_flags("> ```mermaid\n> graph TD\n"), [false]);
}

#[test]
fn a_streamed_fence_closes_with_its_last_line() {
    let source = "intro\n\n```mermaid\ngraph TD\n  A --> B\n```\n";
    let mut p = IncrementalParser::default();
    let closer = source.rfind("```").unwrap();
    p.set_text(&source[..closer]);
    assert!(matches!(
        p.tree().blocks.last().map(|top| &top.block),
        Some(Block::CodeBlock { closed: false, .. })
    ));
    p.set_text(source);
    assert!(matches!(
        p.tree().blocks.last().map(|top| &top.block),
        Some(Block::CodeBlock { closed: true, .. })
    ));
    assert_eq!(p.tree(), &parse_full(source));
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

fn math_run(tex: &str, kind: MathKind) -> InlineRun {
    InlineRun {
        text: tex.into(),
        style: InlineStyle {
            math: Some(kind),
            ..Default::default()
        },
    }
}

fn prose_run(text: &str) -> InlineRun {
    InlineRun {
        text: text.into(),
        style: InlineStyle::default(),
    }
}

fn blocks(source: &str) -> Vec<Block> {
    parse_full(source)
        .blocks
        .into_iter()
        .map(|b| b.block)
        .collect()
}

#[test]
fn inline_math_is_its_own_run() {
    assert_eq!(
        blocks("where $x_1$ and $y$ hold"),
        vec![Block::Paragraph {
            runs: vec![
                prose_run("where "),
                math_run("x_1", MathKind::Inline),
                prose_run(" and "),
                math_run("y", MathKind::Inline),
                prose_run(" hold"),
            ]
        }]
    );
    // Adjacent formulas never merge into one.
    assert_eq!(
        blocks("$a$$b$"),
        vec![Block::Paragraph {
            runs: vec![
                math_run("a", MathKind::Inline),
                math_run("b", MathKind::Inline)
            ]
        }]
    );
}

#[test]
fn display_math_stays_a_run_of_its_paragraph() {
    // One paragraph stays one block (the incremental parser's unit); the
    // renderer gives the formula its own row.
    assert_eq!(
        blocks("so\n$$x$$\nthus"),
        vec![Block::Paragraph {
            runs: vec![
                prose_run("so "),
                math_run("x", MathKind::Display),
                prose_run(" thus"),
            ]
        }]
    );
    assert_eq!(
        blocks("$$\nE = mc^2\n$$"),
        vec![Block::Paragraph {
            runs: vec![math_run("\nE = mc^2\n", MathKind::Display)]
        }]
    );
}

#[test]
fn tex_delimiters_read_as_math() {
    assert_eq!(
        blocks(r"a \(x\) b"),
        vec![Block::Paragraph {
            runs: vec![
                prose_run("a "),
                math_run("x", MathKind::Inline),
                prose_run(" b"),
            ]
        }]
    );
    assert_eq!(
        blocks("\\[\n\\frac{a}{b}\n\\]"),
        vec![Block::Paragraph {
            runs: vec![math_run("\n\\frac{a}{b}\n", MathKind::Display)]
        }]
    );
    // The rewrite keeps every offset: ranges still index the real source.
    let source = "intro\n\n\\[x\\]\n\nend";
    let tree = parse_full(source);
    assert_eq!(source[tree.blocks[1].range.clone()].trim_end(), "\\[x\\]");
}

#[test]
fn tex_delimiters_stay_literal_where_they_are_not_math() {
    // Unpaired, escaped-backslash, inside code, or split across blocks.
    let text = |source: &str| -> String { blocks(source).iter().map(flat).collect() };
    assert_eq!(text(r"just \( a paren"), "just ( a paren");
    assert_eq!(text(r"\\(not\\)"), r"\(not\)");
    assert_eq!(text(r"`\(code\)`"), r"\(code\)");
    assert_eq!(text("\\(a\n\n b\\)"), "(ab)");
    assert!(
        blocks("```\n\\[x\\]\n```")
            .iter()
            .all(|b| matches!(b, Block::CodeBlock { code, .. } if code == "\\[x\\]"))
    );
}

#[test]
fn dollar_prices_stay_prose() {
    assert_eq!(
        blocks("costs $5 and $10 today"),
        vec![Block::Paragraph {
            runs: vec![prose_run("costs $5 and $10 today")]
        }]
    );
}

fn artifact(source: &str) -> (ArtifactKind, Option<String>, String, bool) {
    match blocks(source).into_iter().next() {
        Some(Block::Artifact {
            kind,
            title,
            code,
            closed,
            patch: false,
        }) => (kind, title, code, closed),
        other => panic!("not an artifact: {other:?}"),
    }
}

#[test]
fn patch_fences_name_the_page_they_edit() {
    let source = "```html artifact-patch title=\"Session explainer\"\n<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n```\n";
    assert_eq!(
        blocks(source),
        vec![Block::Artifact {
            kind: ArtifactKind::Html,
            title: Some("Session explainer".to_string()),
            code: "<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE".to_string(),
            closed: true,
            patch: true,
        }]
    );
    assert!(matches!(
        blocks("```Artifact-Patch svg title=Logo\nx\n```").as_slice(),
        [Block::Artifact {
            kind: ArtifactKind::Svg,
            patch: true,
            ..
        }]
    ));
    // A patch to no page in particular, or in a language that doesn't
    // render, is only code.
    for source in [
        "```html artifact-patch\nx\n```",
        "```python artifact-patch title=X\nx\n```",
    ] {
        assert!(
            matches!(blocks(source).as_slice(), [Block::CodeBlock { .. }]),
            "{source}"
        );
    }
}

#[test]
fn artifact_fences_carry_kind_title_and_code() {
    assert_eq!(
        artifact("```html artifact title=\"Bubble sort\"\n<p>a</p>\n<p>b</p>\n```\n"),
        (
            ArtifactKind::Html,
            Some("Bubble sort".to_string()),
            "<p>a</p>\n<p>b</p>".to_string(),
            true
        )
    );
    // The marker can come first; titles may be single-quoted or bare.
    assert_eq!(
        artifact("```artifact svg title='Logo'\n<svg/>\n```").0,
        ArtifactKind::Svg
    );
    assert_eq!(
        artifact("```Artifact HTML title=Plain\nx\n```")
            .1
            .as_deref(),
        Some("Plain")
    );
    // No language: HTML. No title: none.
    assert_eq!(artifact("```artifact\nx\n```").0, ArtifactKind::Html);
    assert_eq!(artifact("```html artifact title=\"  \"\nx\n```").1, None);
}

#[test]
fn only_marked_renderable_fences_are_artifacts() {
    for source in [
        "```html\n<p>x</p>\n```",
        "```python artifact\nprint(1)\n```",
        "```html title=\"artifact\"\n<p>x</p>\n```",
        "```html artifacts\n<p>x</p>\n```",
    ] {
        assert!(
            matches!(blocks(source).as_slice(), [Block::CodeBlock { .. }]),
            "{source}"
        );
    }
}

#[test]
fn artifacts_know_when_their_fence_closed() {
    assert!(!artifact("```html artifact\n<p>x</p>").3);
    assert!(!artifact("```html artifact\n<p>x</p>\n``").3);
    assert!(artifact("```html artifact\n<p>x</p>\n```").3);
    // A longer opening fence needs a closing one as long.
    assert!(!artifact("````html artifact\n```\n").3);
    assert!(artifact("~~~html artifact\nx\n~~~~\n").3);
    // Inside a quote, the markers before the fences don't count.
    match blocks("> ```html artifact\n> <p>x</p>\n> ```\n").as_slice() {
        [Block::BlockQuote { children }] => assert!(matches!(
            children.as_slice(),
            [Block::Artifact { closed: true, .. }]
        )),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_streamed_artifact_closes_when_its_fence_does() {
    let source = "intro\n\n```html artifact title=\"Demo\"\n<p>x</p>\n```\n";
    let mut parser = IncrementalParser::default();
    let fence_end = source.rfind("```").unwrap();
    parser.set_text(&source[..fence_end]);
    assert!(matches!(
        parser.tree().blocks.last().map(|top| &top.block),
        Some(Block::Artifact { closed: false, .. })
    ));
    parser.set_text(source);
    assert!(matches!(
        parser.tree().blocks.last().map(|top| &top.block),
        Some(Block::Artifact { closed: true, .. })
    ));
}
