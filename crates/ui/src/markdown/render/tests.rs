use super::*;

/// Flatten inline runs into shaped-text inputs. Pure given a theme.
fn flatten_runs(runs: &[InlineRun], theme: &Theme, bold_default: bool) -> FlatText {
    flatten_runs_weighted(
        runs,
        theme,
        if bold_default {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        },
    )
}

use crate::markdown::parser::InlineStyle;

#[test]
fn stale_spans_still_cover_the_line_on_char_boundaries() {
    // The editor paints the previous parse until the re-highlight lands:
    // after a deletion its spans overrun the line, after typing CJK they
    // end mid-character. Either used to panic inside gpui's layout.
    let theme = Theme::dark();
    let mono = theme.mono();
    let span = |range: Range<usize>| HighlightSpan {
        range,
        kind: HighlightKind::Keyword,
    };
    for (line, spans) in [
        ("fn", vec![span(0..2), span(3..9)]),
        ("f", vec![span(0..2)]),
        ("你x", vec![span(0..1), span(1..4)]),
        ("aéb", vec![span(2..3), span(1..2)]),
        ("", vec![span(0..5)]),
    ] {
        let runs = runs_for_syntax_line(line, &spans, &mono, &theme);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), line.len());
        let mut at = 0;
        for run in &runs {
            at += run.len;
            assert!(line.is_char_boundary(at), "{line:?}: {at}");
        }
    }
}

#[test]
fn fenced_code_runs_cover_literal_newlines_tabs_and_unicode() {
    let theme = Theme::dark();
    for code in [
        "",
        "\n",
        "x\n\n",
        "\tlet café = \"你好\";\n\n    return café;",
    ] {
        let flat = flatten_code(code, None, &theme);
        assert_eq!(flat.text.as_ref(), code);
        assert_eq!(
            flat.runs.iter().map(|run| run.len).sum::<usize>(),
            code.len()
        );
        assert!(flat.links.is_empty());
        assert!(flat.code_ranges.is_empty());
    }
    let code = "let x = 1;\n\nreturn x;";
    let highlights = vec![
        vec![HighlightSpan {
            range: 0..3,
            kind: HighlightKind::Keyword,
        }],
        vec![],
        vec![HighlightSpan {
            range: 0..6,
            kind: HighlightKind::Keyword,
        }],
    ];
    let flat = flatten_code(code, Some(&highlights), &theme);
    assert_eq!(
        flat.runs.iter().map(|run| run.len).sum::<usize>(),
        code.len()
    );
    assert_eq!(
        flat.runs[0].color,
        theme.syntax.color(HighlightKind::Keyword)
    );
    let mut offset = 0;
    let return_offset = code.find("return").unwrap();
    for run in &flat.runs {
        if offset == return_offset {
            assert_eq!(run.len, 6);
            assert_eq!(run.color, theme.syntax.color(HighlightKind::Keyword));
        }
        offset += run.len;
    }
}

#[gpui::test]
fn fenced_code_drag_selects_across_blank_lines_and_reports_exact_quote(
    cx: &mut gpui::TestAppContext,
) {
    use crate::markdown::selection;
    use gpui::MouseButton;
    let _guard = selection::tests::state_lock();
    let scope = selection::next_side_chat_scope();
    let code = concat!(
        "let answer = 42;\n\n    println!(\"你好\");\n",
        "let long_line = \"012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789\";\n",
    );
    let settled = Rc::new(RefCell::new(None));
    struct CodeView {
        scope: selection::SelectionScope,
        code: &'static str,
        settled: Rc<RefCell<Option<selection::SelectionSnapshot>>>,
    }
    impl gpui::Render for CodeView {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            let mut opts = RenderOptions::settled("selection-code".into());
            opts.scope = self.scope;
            let captured = self.settled.clone();
            opts.selection = Some(SelectionUi {
                on_started: Rc::new(|_, _| {}),
                on_cleared: Rc::new(|_, _| {}),
                on_settled: Rc::new(move |snapshot, _, _, _| {
                    *captured.borrow_mut() = Some(snapshot);
                }),
            });
            div()
                .w(px(380.0))
                .child(selection_frame_reset(self.scope))
                .child(render_code_block(
                    Some("rust"),
                    self.code,
                    BlockCtx::top(0, &opts, &theme),
                    None,
                ))
        }
    }
    cx.update(|cx| cx.set_global(Theme::dark()));
    let window = cx.open_window(gpui::size(px(500.0), px(300.0)), |_, _| CodeView {
        scope,
        code,
        settled: settled.clone(),
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let layout = REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let entries = &registry[&scope];
        assert_eq!(
            entries.len(),
            1,
            "one source model, not disconnected code rows"
        );
        assert_eq!(entries[0].text.as_ref(), code);
        entries[0].layout.clone()
    });
    let from = code.find("answer").unwrap();
    let to = code.find("你好").unwrap() + "你好".len();
    assert!(layout.bounds().size.width > px(380.0));
    assert_eq!(layout.line_layouts().len(), code.split('\n').count());
    let point_for = |index| {
        layout.position_for_index(index).unwrap() + point(px(0.1), layout.line_height() / 2.0)
    };
    let start = point_for(from);
    let end = point_for(to);
    visual.simulate_mouse_down(start, MouseButton::Left, Default::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Default::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Default::default());
    let expected = &code[from..to];
    assert_eq!(selection::selected_text().as_deref(), Some(expected));
    assert_eq!(settled.borrow().as_ref().unwrap().text, expected);
    assert!(range_rects(&layout, &(from..to), 0.0, 0.0).len() >= 2);

    // Reverse dragging must yield the same bytes and preserve indentation.
    visual.simulate_mouse_down(end, MouseButton::Left, Default::default());
    visual.simulate_mouse_move(start, MouseButton::Left, Default::default());
    visual.simulate_mouse_up(start, MouseButton::Left, Default::default());
    assert_eq!(selection::selected_text().as_deref(), Some(expected));

    // Horizontal scroll must move the actual selectable glyph geometry,
    // not just the syntax paint; long lines remain unwrapped.
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: point(px(200.0), end.y),
        delta: gpui::ScrollDelta::Pixels(point(px(-100.0), px(0.0))),
        modifiers: Default::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let scrolled = REGISTRY.with(|registry| registry.borrow()[&scope][0].layout.clone());
    assert!(scrolled.bounds().origin.x < layout.bounds().origin.x);
    let from = code.find("0123456789").unwrap() + 20;
    let to = from + 10;
    let inset = point(px(0.1), scrolled.line_height() / 2.0);
    let start = scrolled.position_for_index(from).unwrap() + inset;
    let end = scrolled.position_for_index(to).unwrap() + inset;
    visual.simulate_mouse_down(start, MouseButton::Left, Default::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Default::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Default::default());
    assert_eq!(selection::selected_text().as_deref(), Some(&code[from..to]));

    visual.simulate_event(gpui::MouseDownEvent {
        position: start,
        modifiers: Default::default(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    visual.simulate_mouse_up(start, MouseButton::Left, Default::default());
    let digits = code.split('"').nth(3).unwrap();
    assert_eq!(selection::selected_text().as_deref(), Some(digits));
    // Hidden overflow must not steal a click beside the code block.
    visual.simulate_click(point(px(420.0), start.y), Default::default());
    assert!(selection::selected_text().is_none());
    selection::clear(scope);
}

#[test]
fn clipped_long_line_cannot_start_selection_in_the_other_diff_column() {
    let text = Bounds::new(point(px(-80.0), px(0.0)), gpui::size(px(900.0), px(21.0)));
    let left_column = Bounds::new(point(px(0.0), px(0.0)), gpui::size(px(200.0), px(300.0)));
    let visible = text.intersect(&left_column);
    assert!(text.contains(&point(px(250.0), px(10.0))));
    assert!(!visible.contains(&point(px(250.0), px(10.0))));
    assert!(visible.contains(&point(px(100.0), px(10.0))));
}
#[test]
fn inline_colors_preserve_prose_fenced_code_and_default_washes() {
    for mut theme in [Theme::dark(), Theme::light()] {
        assert_eq!(inline_code_text(&theme), theme.code_text);
        assert_eq!(inline_code_wash(&theme), theme.code_wash);
        let plain_code_color = code_block_theme(&theme).text;
        let fenced_background = code_block_background(&theme);
        let foreground = gpui::rgb(0xaa2266).into();
        let background = gpui::rgb(0xeeeedd).into();
        theme.inline_code_text = Some(foreground);
        theme.inline_code_background = Some(background);
        assert_eq!(inline_code_text(&theme), foreground);
        assert_eq!(inline_code_wash(&theme), background);
        assert_eq!(code_block_theme(&theme).text, plain_code_color);
        assert_eq!(code_block_background(&theme), fenced_background);
        let code_run = InlineRun {
            text: "inline".into(),
            style: InlineStyle {
                code: true,
                ..Default::default()
            },
        };
        let flat = flatten_runs(&[code_run], &theme, false);
        assert_eq!(flat.runs[0].color, foreground);
        assert_eq!(flat.code_ranges, vec![0..6]);
        let prose = flatten_runs(
            &[InlineRun {
                text: "prose".into(),
                style: InlineStyle::default(),
            }],
            &theme,
            false,
        );
        assert_eq!(prose.runs[0].color, theme.text);
    }
}

#[test]
fn runs_text_matches_the_flattened_text() {
    // The find index counts over `runs_text`; the painter highlights over
    // the flattened `FlatText`. If these two ever disagree the match
    // ordinals drift and the wrong hit lights up, so pin them together
    // over the boundaries that make the two differ: empty runs (dropped)
    // and prose/code transitions (a thin space is inserted OUTSIDE the
    // code range).
    let code = |text: &str| InlineRun {
        text: text.into(),
        style: InlineStyle {
            code: true,
            ..Default::default()
        },
    };
    let prose = |text: &str| InlineRun {
        text: text.into(),
        style: InlineStyle::default(),
    };
    let theme = Theme::dark();
    for runs in [
        vec![],
        vec![prose("plain prose")],
        vec![code("cypher")],
        vec![prose("call "), code("cypher"), prose(" twice")],
        // Adjacent code runs merge into one wash but no margin between.
        vec![code("cy"), code("pher"), prose(" done")],
        // Empty runs never reach the text.
        vec![prose(""), prose("a"), code(""), code("b"), prose("")],
    ] {
        assert_eq!(
            runs_text(&runs),
            flatten_runs(&runs, &theme, false).text.as_ref(),
        );
    }
}

#[test]
fn block_match_counts_walk_every_rendered_element() {
    let runs = |text: &str| {
        vec![InlineRun {
            text: text.into(),
            style: InlineStyle::default(),
        }]
    };
    assert_eq!(count_block_matches(&Block::Rule, "x"), 0);
    assert_eq!(
        count_block_matches(
            &Block::Paragraph {
                runs: runs("x y x")
            },
            "x"
        ),
        2
    );
    assert_eq!(
        count_block_matches(
            &Block::CodeBlock {
                language: None,
                code: "let x = x;\nreturn x;".into(),
            },
            "x"
        ),
        3
    );
    // Nested blocks recurse; table header and body cells both count.
    let nested = Block::List {
        ordered_start: None,
        items: vec![
            vec![Block::Paragraph { runs: runs("x") }],
            vec![Block::BlockQuote {
                children: vec![Block::Heading {
                    level: 2,
                    runs: runs("x x"),
                }],
            }],
        ],
    };
    assert_eq!(count_block_matches(&nested, "x"), 3);
    let table = Block::Table {
        header: vec![runs("x"), runs("y")],
        rows: vec![vec![runs("x x"), runs("y")], vec![runs("x")]],
        align: Vec::new(),
    };
    assert_eq!(count_block_matches(&table, "x"), 4);
}

#[test]
fn inline_color_changes_refresh_cached_runs() {
    let cache = Rc::new(RefCell::new(RenderCache::default()));
    let mut opts = RenderOptions::settled("inline-color-cache".into());
    opts.cache = Some(cache);
    let runs = [InlineRun {
        text: "value".into(),
        style: InlineStyle {
            code: true,
            ..Default::default()
        },
    }];
    let mut theme = Theme::dark();
    theme.text_style_revision = 1;
    let before = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &theme);
    theme.inline_code_text = Some(gpui::rgb(0xffcc88).into());
    theme.inline_code_background = Some(gpui::rgb(0x332200).into());
    theme.text_style_revision = 2;
    let after = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &theme);
    assert!(!Rc::ptr_eq(&before, &after));
    assert_eq!(after.runs[0].color, theme.inline_code_text.unwrap());
    assert_eq!(
        before.code_ranges, after.code_ranges,
        "color changes must not alter layout"
    );
    assert_eq!(before.text, after.text);
}

#[test]
fn code_block_base_color_preserves_colored_syntax_and_other_renderers() {
    for mut theme in [Theme::dark(), Theme::light()] {
        assert_eq!(code_block_background(&theme), theme.ink(0.035));
        let foreground = gpui::rgb(0x6655aa).into();
        theme.code_block_text = Some(foreground);
        theme.code_block_background = Some(gpui::rgb(0xf0f0f0).into());
        let code = code_block_theme(&theme);
        assert_eq!(
            code_block_background(&theme),
            theme.code_block_background.unwrap()
        );
        for kind in [
            HighlightKind::Variable,
            HighlightKind::Parameter,
            HighlightKind::Operator,
            HighlightKind::Punctuation,
            HighlightKind::Embedded,
        ] {
            assert_eq!(token_color(kind, &code), foreground);
            assert_eq!(token_color(kind, &theme), theme.text);
        }
        for kind in [
            HighlightKind::Keyword,
            HighlightKind::String,
            HighlightKind::Comment,
            HighlightKind::Number,
            HighlightKind::Function,
            HighlightKind::Type,
            HighlightKind::Boolean,
            HighlightKind::VariableSpecial,
        ] {
            assert_eq!(token_color(kind, &code), token_color(kind, &theme));
        }
        let spans = [
            HighlightSpan {
                range: 0..3,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                range: 4..7,
                kind: HighlightKind::Variable,
            },
            HighlightSpan {
                range: 8..11,
                kind: HighlightKind::String,
            },
        ];
        let mono = theme.mono();
        let runs = runs_for_syntax_line("abc def ghi", &spans, &mono, &code);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), 11);
        assert_eq!(
            runs.iter().map(|r| r.color).collect::<Vec<_>>(),
            vec![
                theme.syntax.keyword,
                foreground,
                foreground,
                foreground,
                theme.syntax.string,
            ]
        );
        let ordinary = runs_for_syntax_line("plain", &[], &mono, &theme);
        assert_eq!(
            ordinary[0].color, theme.text,
            "generic/diff rendering must not use the fenced override"
        );
        assert_eq!(inline_code_text(&code), inline_code_text(&theme));
    }
}

#[test]
fn code_color_change_rebuilds_cached_code_runs() {
    let cache = Rc::new(RefCell::new(RenderCache::default()));
    let mut opts = RenderOptions::settled("code-color-cache".into());
    opts.cache = Some(cache.clone());
    let mut theme = Theme::dark();
    theme.text_style_revision = 1;
    theme.code_block_text = Some(gpui::rgb(0xffccaa).into());
    let _ = render_code_block(None, "plain", BlockCtx::top(0, &opts, &theme), None);
    let key = (opts.row_key.clone(), 0, 0);
    let before = cache.borrow().code[&key].clone();
    theme.text_style_revision = 2;
    theme.code_block_text = Some(gpui::rgb(0xaaccff).into());
    let _ = render_code_block(None, "plain", BlockCtx::top(0, &opts, &theme), None);
    let after = cache.borrow().code[&key].clone();
    assert!(!Rc::ptr_eq(&before, &after));
    assert_eq!(before.flat.runs[0].color, gpui::rgb(0xffccaa).into());
    assert_eq!(after.flat.runs[0].color, theme.code_block_text.unwrap());
}

#[test]
fn chat_style_revision_invalidates_cached_fonts_and_colors() {
    let cache = Rc::new(RefCell::new(RenderCache::default()));
    let mut opts = RenderOptions::settled("style-cache".into());
    opts.cache = Some(cache.clone());
    let runs = vec![InlineRun {
        text: "link".into(),
        style: InlineStyle {
            link: Some("https://example.com".into()),
            ..Default::default()
        },
    }];
    let mut old = Theme::dark();
    old.text_style_revision = 1;
    let before = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &old);
    let mut new = old.clone();
    new.text_style_revision = 2;
    new.font_sans = "Test Serif".into();
    new.markdown_link = Some(gpui::rgb(0x3388cc).into());
    let after = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &new);
    assert!(!Rc::ptr_eq(&before, &after));
    assert_eq!(after.runs[0].font.family.as_ref(), "Test Serif");
    assert_eq!(after.runs[0].color, new.markdown_link.unwrap());
    assert_eq!(before.runs[0].font.family, old.font_sans);
}

#[test]
fn code_line_runs_cover_exactly() {
    let theme = Theme::dark();
    let mono = theme.mono();
    let line = r#"let x = "hi"; // done"#;
    let document = cypher_syntax::highlight(cypher_syntax::HighlightRequest {
        source: line,
        path: None,
        fence_tag: Some("rust"),
    })
    .unwrap();
    let runs = runs_for_syntax_line(line, &document.lines[0], &mono, &theme);
    let total: usize = runs.iter().map(|r| r.len).sum();
    assert_eq!(total, line.len());
    assert!(
        runs.iter().all(|r| r.font == mono),
        "highlight must not change fonts"
    );
    // At least one non-plain color made it through.
    assert!(runs.iter().any(|r| r.color != theme.text));
}

#[test]
fn tree_sitter_runs_are_rich_and_paint_only() {
    let theme = Theme::dark();
    let mono = theme.mono();
    let line = "let widget = build!(42);";
    let document = cypher_syntax::highlight(cypher_syntax::HighlightRequest {
        source: line,
        path: None,
        fence_tag: Some("rust"),
    })
    .unwrap();
    let runs = runs_for_syntax_line(line, &document.lines[0], &mono, &theme);
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), line.len());
    assert!(runs.iter().all(|run| run.font == mono));
    let colors = runs.iter().map(|run| run.color).collect::<Vec<_>>();
    assert!(colors.contains(&theme.syntax.keyword));
    assert!(colors.contains(&theme.syntax.macro_name));
    assert!(colors.contains(&theme.syntax.number));
}

#[test]
fn code_line_runs_with_no_tokens_are_one_plain_run() {
    let theme = Theme::dark();
    let mono = theme.mono();
    let runs = runs_for_syntax_line("plain text", &[], &mono, &theme);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].len, 10);
}

#[test]
fn flatten_collects_and_merges_inline_code_ranges() {
    let theme = Theme::dark();
    let code = |text: &str| InlineRun {
        text: text.into(),
        style: InlineStyle {
            code: true,
            ..Default::default()
        },
    };
    let plain = |text: &str| InlineRun {
        text: text.into(),
        style: InlineStyle::default(),
    };
    let flat = flatten_runs(
        &[
            plain("use "),
            code("foo"),
            code("()"),
            plain(" and "),
            code("bar"),
        ],
        &theme,
        false,
    );
    // Thin margins separate code from prose without entering the wash;
    // adjacent code runs still merge into ONE box.
    assert_eq!(flat.text, "use \u{2009}foo()\u{2009} and \u{2009}bar");
    assert_eq!(flat.code_ranges, vec![7..12, 23..26]);
    // Code text is the emerald tint; the square run background is gone
    // (the rounded wash is painted by the canvas underlay instead).
    assert_eq!(flat.runs[2].color, inline_code_text(&theme));
    assert_eq!(flat.runs[2].background_color, None);
    assert_eq!(flat.runs[0].color, theme.text);
}

#[test]
fn code_palette_is_colored_and_shared() {
    // Transcript code blocks paint the soft hues (rose keyword,
    // green string, amber number); comments stay faint neutral.
    let theme = Theme::dark();
    assert_ne!(token_color(HighlightKind::Keyword, &theme), theme.text);
    assert_ne!(
        token_color(HighlightKind::String, &theme),
        token_color(HighlightKind::Keyword, &theme)
    );
    assert_ne!(token_color(HighlightKind::Comment, &theme), theme.text);
}

#[test]
fn flatten_runs_maps_links_and_styles() {
    let theme = Theme::dark();
    let runs = vec![
        InlineRun {
            text: "go ".into(),
            style: InlineStyle::default(),
        },
        InlineRun {
            text: "here".into(),
            style: InlineStyle {
                link: Some("https://x.dev".into()),
                ..Default::default()
            },
        },
        InlineRun {
            text: " now".into(),
            style: InlineStyle {
                bold: true,
                ..Default::default()
            },
        },
    ];
    let flat = flatten_runs(&runs, &theme, false);
    assert_eq!(flat.text, "go here now");
    assert_eq!(flat.links, vec![(3..7, "https://x.dev".to_string())]);
    let total: usize = flat.runs.iter().map(|r| r.len).sum();
    assert_eq!(total, flat.text.len());
    // Links stay monochrome (foreground + underline), never accent-tinted.
    assert_eq!(flat.runs[1].color, theme.text);
    assert!(flat.runs[1].underline.is_some());
    assert_eq!(flat.runs[2].font.weight, FontWeight::SEMIBOLD);
}

/// Model GPUI's upstream affinity at a soft-wrap boundary: byte 5 is
/// reported at the end of row 0, while byte 6 is on row 1.
fn wrapped_position(ix: usize) -> Option<gpui::Point<gpui::Pixels>> {
    (ix <= 9).then(|| {
        if ix <= 5 {
            point(px(ix as f32 * 10.0), px(0.0))
        } else {
            point(px((ix - 5) as f32 * 10.0), px(22.0))
        }
    })
}

fn wrapped_range_rects(range: Range<usize>) -> Vec<Bounds<gpui::Pixels>> {
    range_rects_with_positions(
        Bounds::new(point(px(0.0), px(0.0)), size(px(50.0), px(44.0))),
        px(22.0),
        &range,
        0.0,
        0.0,
        wrapped_position,
    )
}

#[test]
fn inline_code_wash_follows_font_size_not_line_height() {
    // Default metrics keep the original 2px inset (18px wash).
    assert!((inline_code_inset_y(22.0, 14.0) - 2.0).abs() < 1e-4);
    // A taller line only adds inset; the wash height stays 18px.
    let inset = inline_code_inset_y(32.0, 14.0);
    assert!((32.0 - 2.0 * inset - 18.0).abs() < 1e-4);
    // A tighter line than the wash never goes negative.
    assert_eq!(inline_code_inset_y(16.0, 14.0), 0.0);
}

#[test]
fn range_starting_at_soft_wrap_includes_first_glyph() {
    let rects = wrapped_range_rects(5..9);
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].origin, point(px(0.0), px(22.0)));
    assert_eq!(rects[0].size, size(px(40.0), px(22.0)));
}

#[test]
fn range_crossing_soft_wrap_includes_first_continuation_glyph() {
    let rects = wrapped_range_rects(2..9);
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0].origin, point(px(20.0), px(0.0)));
    assert_eq!(rects[0].size, size(px(30.0), px(22.0)));
    assert_eq!(rects[1].origin, point(px(0.0), px(22.0)));
    assert_eq!(rects[1].size, size(px(40.0), px(22.0)));
}

#[test]
fn table_columns_floor_and_padding() {
    // A short column keeps its content width (floored at MIN_COLUMN_CONTENT
    // + padding); a wide one may wrap but no narrower than minColumnWidth.
    let geo = table_columns(&[10.0, 200.0]);
    assert_eq!(geo.naturals, vec![72.0, 224.0]); // 48+24, 200+24
    assert_eq!(geo.minimums, vec![72.0, 96.0]);
    assert_eq!(geo.min_table_width, 168.0);
}

#[test]
fn table_columns_are_content_proportional_not_equal() {
    let geo = table_columns(&[300.0, 60.0, 60.0]);
    // Flex grow factors are the naturals — a prose column gets a larger
    // share than short ones (not equal thirds).
    assert!(geo.naturals[0] > 3.0 * geo.naturals[1] * 0.9);
    assert_eq!(geo.naturals[1], geo.naturals[2]);
}

#[test]
fn table_header_flattens_at_weight_700() {
    let theme = Theme::dark();
    let runs = vec![InlineRun {
        text: "Header".into(),
        style: InlineStyle::default(),
    }];
    let flat = flatten_runs_weighted(&runs, &theme, TABLE_HEADER_WEIGHT);
    assert_eq!(flat.runs[0].font.weight, FontWeight::BOLD);
    // Strong runs inside a 700 header stay 700 (never drop to semibold).
    let bold_runs = vec![InlineRun {
        text: "Strong".into(),
        style: InlineStyle {
            bold: true,
            ..Default::default()
        },
    }];
    let flat = flatten_runs_weighted(&bold_runs, &theme, TABLE_HEADER_WEIGHT);
    assert_eq!(flat.runs[0].font.weight, FontWeight::BOLD);
}

#[test]
fn adjacent_same_link_runs_merge_into_one_range() {
    let theme = Theme::dark();
    let style = InlineStyle {
        link: Some("https://x.dev".into()),
        ..Default::default()
    };
    let runs = vec![
        InlineRun {
            text: "bold".into(),
            style: InlineStyle {
                bold: true,
                ..style.clone()
            },
        },
        InlineRun {
            text: " tail".into(),
            style,
        },
    ];
    let flat = flatten_runs(&runs, &theme, false);
    assert_eq!(flat.links, vec![(0..9, "https://x.dev".to_string())]);
}
