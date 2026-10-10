use super::languages::rust_configuration;
use super::*;

#[test]
fn aliases_keep_language_variants_distinct() {
    let cases = [
        ("js", LanguageId::JavaScript),
        ("jsx", LanguageId::Jsx),
        ("ts", LanguageId::TypeScript),
        ("tsx", LanguageId::Tsx),
        ("RS", LanguageId::Rust),
        ("shell", LanguageId::Bash),
    ];
    for (alias, expected) in cases {
        assert_eq!(language_for_alias(alias), Some(expected), "{alias}");
    }
    assert_eq!(language_for_alias("unknown-lang"), None);
}

#[test]
fn paths_and_exact_names_are_table_driven() {
    let cases = [
        ("src/main.rs", LanguageId::Rust),
        ("web/app.tsx", LanguageId::Tsx),
        ("Cargo.toml", LanguageId::Toml),
        ("Dockerfile", LanguageId::Dockerfile),
        ("GNUmakefile", LanguageId::Make),
        ("config.jsonc", LanguageId::Jsonc),
    ];
    for (path, expected) in cases {
        assert_eq!(language_for_path(path), Some(expected), "{path}");
    }
    assert_eq!(language_for_path("README"), None);
    assert_eq!(language_for_path("image.png"), None);
}

#[test]
fn shebang_is_only_used_after_explicit_hints() {
    assert_eq!(
        detect_language(None, None, Some("#!/usr/bin/env python3")),
        Some(LanguageId::Python)
    );
    assert_eq!(detect_language(None, None, Some("let x = 1")), None);
}

#[test]
fn spans_are_valid_sorted_non_overlapping_and_line_relative() {
    let source = "let café = \"x\";\nnext";
    let document = HighlightedDocument::from_absolute_spans(
        LanguageId::Rust,
        source,
        [
            HighlightSpan {
                range: 0..9,
                kind: HighlightKind::Variable,
            },
            HighlightSpan {
                range: 0..3,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                range: 12..15,
                kind: HighlightKind::String,
            },
            HighlightSpan {
                range: 17..21,
                kind: HighlightKind::Function,
            },
        ],
    )
    .unwrap();
    assert_eq!(document.lines.len(), 2);
    assert_eq!(
        document.lines[0][0],
        HighlightSpan {
            range: 0..3,
            kind: HighlightKind::Keyword
        }
    );
    for line in document.lines {
        assert!(
            line.windows(2)
                .all(|pair| pair[0].range.end <= pair[1].range.start)
        );
    }
    assert_eq!(
        HighlightedDocument::from_absolute_spans(
            LanguageId::Rust,
            source,
            [HighlightSpan {
                range: 8..9,
                kind: HighlightKind::Type
            }]
        ),
        Err(HighlightError::InvalidUtf8Boundary { start: 8, end: 9 })
    );
}

#[test]
fn normalization_preserves_overlap_precedence_and_tie_order() {
    let normalized = normalize_line(vec![
        HighlightSpan {
            range: 0..10,
            kind: HighlightKind::Variable,
        },
        HighlightSpan {
            range: 2..8,
            kind: HighlightKind::Keyword,
        },
        HighlightSpan {
            range: 4..6,
            kind: HighlightKind::String,
        },
    ]);
    assert_eq!(
        normalized,
        vec![
            HighlightSpan {
                range: 0..2,
                kind: HighlightKind::Variable,
            },
            HighlightSpan {
                range: 2..4,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                range: 4..6,
                kind: HighlightKind::String,
            },
            HighlightSpan {
                range: 6..8,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                range: 8..10,
                kind: HighlightKind::Variable,
            },
        ]
    );
}

#[test]
fn minified_lines_normalize_without_quadratic_rescans() {
    let source = "let value=1;".repeat(8_000);
    let document = highlight(HighlightRequest {
        source: &source,
        path: Some("bundle.js"),
        fence_tag: None,
    })
    .unwrap();
    assert!(document.lines[0].len() > 20_000);
}

fn highlighted_fragments(source: &str) -> Vec<(&str, HighlightKind)> {
    let document = highlight(HighlightRequest {
        source,
        path: Some("src/lib.rs"),
        fence_tag: None,
    })
    .unwrap();
    source
        .lines()
        .zip(document.lines)
        .flat_map(|(line, spans)| {
            spans
                .into_iter()
                .map(move |span| (&line[span.range], span.kind))
        })
        .collect()
}

#[test]
fn rust_highlighting_distinguishes_structural_categories() {
    let source = r#"pub struct Widget { field: usize }
fn build(value: usize) -> Widget {
let name = format!("item-{value}");
Widget { field: 42 }
}"#;
    let fragments = highlighted_fragments(source);
    for (text, expected) in [
        ("pub", HighlightKind::Keyword),
        ("Widget", HighlightKind::Type),
        ("build", HighlightKind::Function),
        ("format!", HighlightKind::Macro),
        ("42", HighlightKind::Number),
    ] {
        assert!(
            fragments.contains(&(text, expected)),
            "missing {text:?} as {expected:?}: {fragments:?}"
        );
    }
}

#[test]
fn rust_multiline_raw_unicode_and_incomplete_code_remain_valid() {
    let source =
        "/* café\ncomment */\nlet raw = r#\"héllo\nworld\"#;\nlet before = 7;\nfn incomplete( {";
    let document = highlight(HighlightRequest {
        source,
        path: None,
        fence_tag: Some("rust"),
    })
    .unwrap();
    assert!(
        document.lines[1]
            .iter()
            .any(|span| span.kind == HighlightKind::Comment)
    );
    assert!(
        document.lines[3]
            .iter()
            .any(|span| span.kind == HighlightKind::String)
    );
    assert!(
        document
            .lines
            .iter()
            .flatten()
            .any(|span| span.kind == HighlightKind::Number)
    );
    for (line, spans) in source.lines().zip(&document.lines) {
        for span in spans {
            assert!(line.is_char_boundary(span.range.start));
            assert!(line.is_char_boundary(span.range.end));
        }
    }
}

#[test]
fn limits_and_unbundled_languages_degrade_with_typed_errors() {
    assert_eq!(
        highlight_with_limits(
            HighlightRequest {
                source: "fn main() {}",
                path: Some("main.rs"),
                fence_tag: None,
            },
            HighlightLimits {
                max_source_bytes: 2,
                max_spans: 10
            },
            None,
        ),
        Err(HighlightError::SourceTooLarge)
    );
    assert_eq!(
        highlight(HighlightRequest {
            source: "plain",
            path: Some("unknown.extension"),
            fence_tag: None,
        }),
        Err(HighlightError::UnknownLanguage)
    );
}

#[test]
fn rust_queries_load_for_the_bundled_abi() {
    assert!(rust_configuration().is_ok());
    let language_version = std::hint::black_box(tree_sitter::LANGUAGE_VERSION);
    assert!(language_version >= tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION);
}

#[test]
fn every_registered_grammar_loads_and_highlights_a_fixture() {
    let fixtures = [
        (LanguageId::JavaScript, "app.js", "const value = call(42);"),
        (
            LanguageId::Jsx,
            "app.jsx",
            "const view = <main id=\"x\" />;",
        ),
        (
            LanguageId::TypeScript,
            "app.ts",
            "const value: number = 42;",
        ),
        (
            LanguageId::Tsx,
            "app.tsx",
            "const view: JSX.Element = <main />;",
        ),
        (
            LanguageId::Python,
            "app.py",
            "def call(value):\n    return value",
        ),
        (LanguageId::Go, "main.go", "package main\nfunc main() {}"),
        (LanguageId::Json, "a.json", "{\"value\": 42}"),
        (LanguageId::Jsonc, "a.jsonc", "{\"value\": 42}"),
        (LanguageId::Bash, "run.sh", "echo \"hello\""),
        (LanguageId::Toml, "Cargo.toml", "name = \"cypher\""),
        (LanguageId::Markdown, "README.md", "# Heading\n\n`code`"),
        (LanguageId::Html, "index.html", "<main id=\"app\"></main>"),
        (LanguageId::Css, "app.css", ".app { color: red; }"),
        (LanguageId::Yaml, "app.yml", "name: cypher"),
        (LanguageId::C, "main.c", "int main(void) { return 0; }"),
        (LanguageId::Cpp, "main.cpp", "int main() { return 0; }"),
        (LanguageId::CSharp, "App.cs", "class App { int Value = 1; }"),
        (LanguageId::Java, "App.java", "class App { int value = 1; }"),
        (LanguageId::Kotlin, "App.kt", "val value = 1"),
        (LanguageId::Swift, "App.swift", "let value: Int = 1"),
        (LanguageId::Ruby, "app.rb", "def call(value)\n value\nend"),
        (
            LanguageId::Php,
            "app.php",
            "<?php function call() { return 1; }",
        ),
        (LanguageId::Sql, "query.sql", "SELECT name FROM users;"),
        (LanguageId::Lua, "app.lua", "local value = 1"),
        (LanguageId::Nix, "flake.nix", "{ pkgs }: pkgs.hello"),
        (LanguageId::Make, "Makefile", "all:\n\techo hello"),
        (
            LanguageId::Dockerfile,
            "Dockerfile",
            "FROM alpine\nRUN echo hello",
        ),
    ];
    for (language, path, source) in fixtures {
        let config =
            configuration(language).unwrap_or_else(|error| panic!("{language:?}: {error}"));
        assert!(
            !config.names().is_empty(),
            "{language:?} query has no captures"
        );
        let document = highlight(HighlightRequest {
            source,
            path: Some(path),
            fence_tag: None,
        })
        .unwrap_or_else(|error| panic!("{language:?}: {error}"));
        assert_eq!(document.language, language);
        assert!(
            document.lines.iter().flatten().next().is_some(),
            "{language:?} fixture has no structural spans"
        );
    }
}

#[test]
fn html_injects_javascript_and_css_with_a_bounded_registry() {
    let source = r#"<main id="app">
<style>.item { color: red; }</style>
<script>const answer = call(42);</script>
</main>"#;
    let document = highlight(HighlightRequest {
        source,
        path: Some("index.html"),
        fence_tag: None,
    })
    .unwrap();
    let kinds = document
        .lines
        .iter()
        .flatten()
        .map(|span| span.kind)
        .collect::<Vec<_>>();
    assert!(kinds.contains(&HighlightKind::Tag));
    assert!(kinds.contains(&HighlightKind::Attribute));
    assert!(kinds.contains(&HighlightKind::Keyword));
    assert!(kinds.contains(&HighlightKind::Number));
}

#[test]
fn jsonc_accepts_and_highlights_comments() {
    let source = "{\n  // explanation\n  \"enabled\": true\n}\n";
    let document = highlight(HighlightRequest {
        source,
        path: Some("settings.jsonc"),
        fence_tag: None,
    })
    .unwrap();
    assert_eq!(document.language, LanguageId::Jsonc);
    assert!(
        document.lines[1]
            .iter()
            .any(|span| span.kind == HighlightKind::Comment)
    );
}

#[test]
fn unknown_markdown_fence_does_not_break_parent_highlighting() {
    let source = "# Title\n\n```unknown-language\nopaque\n```\n";
    let document = highlight(HighlightRequest {
        source,
        path: Some("README.md"),
        fence_tag: None,
    })
    .unwrap();
    assert_eq!(document.language, LanguageId::Markdown);
}

#[test]
fn markdown_fences_use_all_bundled_child_grammars() {
    let source = "```rust\nfn main() { let value = 42; }\n```\n\n```yaml\nenabled: true\n```\n";
    let document = highlight(HighlightRequest {
        source,
        path: Some("README.md"),
        fence_tag: None,
    })
    .unwrap();
    assert!(
        document.lines[1]
            .iter()
            .any(|span| span.kind == HighlightKind::Keyword),
        "Rust fence was not injected: {:?}",
        document.lines[1]
    );
    assert!(
        document.lines[5].iter().any(|span| matches!(
            span.kind,
            HighlightKind::Property | HighlightKind::Boolean | HighlightKind::String
        )),
        "YAML fence was not injected: {:?}",
        document.lines[5]
    );
}
