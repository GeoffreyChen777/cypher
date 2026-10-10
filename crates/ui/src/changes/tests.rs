use super::*;
use chrono::Utc;
use cypher_proto::Chat;
use cypher_syntax::LanguageId as Lang;

const PATCH: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    let x = 1;
 }
@@ -10,2 +11,2 @@
 // tail
-old_line
+new_line
diff --git a/added.txt b/added.txt
new file mode 100644
--- /dev/null
+++ b/added.txt
@@ -0,0 +1,2 @@
+first
+second
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
diff --git a/img.png b/img.png
new file mode 100644
Binary files /dev/null and b/img.png differ
diff --git a/old_name.rs b/new_name.rs
similarity index 90%
rename from old_name.rs
rename to new_name.rs
";

#[test]
fn diff_line_keys_are_owner_scoped_and_stable() {
    // The same pane + line renders the same key every frame (selection
    // continuity while scrolling/virtualizing), and two panes never
    // collide even for identical file/hunk/line indices.
    let key = diff_line_key("changes-1", 0, 0, 3);
    assert_eq!(key, "changes-1:f0:h0:l3");
    assert_eq!(diff_line_key("changes-1", 0, 0, 3), key, "stable");
    assert_ne!(diff_line_key("changes-2", 0, 0, 3), key, "different pane");
    assert_ne!(diff_line_key("changes-1", 1, 0, 3), key, "different file");
    assert_ne!(diff_line_key("changes-1", 0, 2, 3), key, "different hunk");
    assert_ne!(diff_line_key("changes-1", 0, 0, 4), key, "different line");
    // The key survives the snapshot's head-row parser whole (no `-tN` /
    // `:N` renderer suffix to strip) — liveness stays pane-scoped.
    let snapshot = crate::markdown::selection::SelectionSnapshot {
        head_key: key.clone(),
        head_ix: 0,
        spans: Vec::new(),
        text: String::new(),
    };
    assert_eq!(snapshot.head_row(), key);
}

#[test]
fn parses_files_hunks_and_lines() {
    let files = parse_patch(PATCH);
    assert_eq!(files.len(), 5);

    let main = &files[0];
    assert_eq!(main.path, "src/main.rs");
    assert_eq!(main.status, FileStatus::Modified);
    assert_eq!(main.hunks.len(), 2);
    assert_eq!(main.additions, 3);
    assert_eq!(main.deletions, 2);
    let h0 = &main.hunks[0];
    assert_eq!(h0.header, "@@ -1,4 +1,5 @@ fn main");
    assert_eq!(h0.lines.len(), 5);
    assert_eq!(h0.lines[0].kind, LineKind::Context);
    assert_eq!(h0.lines[0].old_no, Some(1));
    assert_eq!(h0.lines[0].new_no, Some(1));
    assert_eq!(h0.lines[1].kind, LineKind::Del);
    assert_eq!(h0.lines[1].old_no, Some(2));
    assert_eq!(h0.lines[1].new_no, None);
    assert_eq!(h0.lines[2].kind, LineKind::Add);
    assert_eq!(h0.lines[2].new_no, Some(2));
    assert_eq!(h0.lines[3].kind, LineKind::Add);
    assert_eq!(h0.lines[3].new_no, Some(3));
    // Closing context line: numbering advanced past the add/del block.
    assert_eq!(h0.lines[4].old_no, Some(3));
    assert_eq!(h0.lines[4].new_no, Some(4));
    // Second hunk restarts numbering from its header.
    assert_eq!(main.hunks[1].lines[0].old_no, Some(10));
    assert_eq!(main.hunks[1].lines[0].new_no, Some(11));
}

#[test]
fn detects_new_deleted_binary_and_renamed() {
    let files = parse_patch(PATCH);
    let added = &files[1];
    assert_eq!(added.status, FileStatus::Added);
    assert_eq!(added.additions, 2);
    // The no-newline marker rides as a Meta line.
    let last = added.hunks[0].lines.last().unwrap();
    assert_eq!(last.kind, LineKind::Meta);
    assert!(last.text.contains("No newline"));
    assert!(file_notices(added).iter().any(|n| n == "New file"));

    let deleted = &files[2];
    assert_eq!(deleted.status, FileStatus::Deleted);
    assert_eq!(deleted.deletions, 1);
    assert!(file_notices(deleted).iter().any(|n| n == "Deleted file"));

    let binary = &files[3];
    assert!(binary.binary);
    assert_eq!(binary.status, FileStatus::Added);
    assert!(binary.hunks.is_empty());
    assert!(file_notices(binary).iter().any(|n| n.contains("Binary")));

    let renamed = &files[4];
    assert_eq!(renamed.status, FileStatus::Renamed);
    assert_eq!(renamed.path, "new_name.rs");
    assert_eq!(renamed.old_path.as_deref(), Some("old_name.rs"));
    assert!(
        file_notices(renamed)
            .iter()
            .any(|n| n.contains("old_name.rs"))
    );
}

#[test]
fn empty_and_garbage_patches_parse_to_nothing() {
    assert!(parse_patch("").is_empty());
    assert!(parse_patch("not a diff\nat all\n").is_empty());
    // Truncated mid-hunk: keeps what parsed.
    let files = parse_patch("diff --git a/x b/x\n@@ -1,9 +1,9 @@\n ctx\n+add");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].hunks[0].lines.len(), 2);
    assert_eq!(files[0].additions, 1);
}

#[test]
fn quoted_and_spaced_paths() {
    let (old, new) = parse_git_paths("a/simple.rs b/simple.rs");
    assert_eq!((old.as_str(), new.as_str()), ("simple.rs", "simple.rs"));
    let (old, new) = parse_git_paths("\"a/with space.rs\" \"b/with space.rs\"");
    assert_eq!(old, "with space.rs");
    assert_eq!(new, "with space.rs");
}

#[test]
fn hunk_headers_parse_with_and_without_counts() {
    assert_eq!(parse_hunk_header("@@ -1,4 +2,5 @@"), Some((1, 2)));
    assert_eq!(parse_hunk_header("@@ -7 +9 @@ fn ctx"), Some((7, 9)));
    assert_eq!(parse_hunk_header("@@ garbage"), None);
}

#[test]
fn rows_flatten_to_line_granularity() {
    let files = parse_patch(PATCH);
    let (rows, ranges) = flatten_rows(&files, |_| false);
    assert_eq!(ranges.len(), files.len());
    // Every file's span starts with its header…
    for (ix, range) in ranges.iter().enumerate() {
        assert_eq!(rows[range.start], DiffRow::FileHeader { file: ix as u32 });
        // …and spans exactly header + analytic body rows.
        assert_eq!(range.len(), 1 + body_row_count(&files[ix]));
    }
    // Spans tile the whole row vec.
    assert_eq!(ranges.last().unwrap().end, rows.len());

    // src/main.rs: header, 2 hunk headers, 8 lines, pad.
    let main_rows = &rows[ranges[0].clone()];
    assert_eq!(main_rows.len(), 1 + 2 + 8 + 1);
    assert_eq!(main_rows[1], DiffRow::HunkHeader { file: 0, hunk: 0 });
    // Flat line indices run across hunks (they key the highlight slot).
    let flats: Vec<u32> = main_rows
        .iter()
        .filter_map(|r| match r {
            DiffRow::Line { flat, .. } => Some(*flat),
            _ => None,
        })
        .collect();
    assert_eq!(flats, (0..8).collect::<Vec<u32>>());
    assert_eq!(*main_rows.last().unwrap(), DiffRow::BodyPad { file: 0 });

    // A collapsed file contributes its header row only.
    let (rows, ranges) = flatten_rows(&files, |ix| ix == 0);
    assert_eq!(ranges[0].len(), 1);
    assert_eq!(rows[ranges[1].start], DiffRow::FileHeader { file: 1 });

    // Notices lead the body: the added file carries "New file".
    let added_rows = &rows[ranges[1].clone()];
    assert_eq!(added_rows[1], DiffRow::Notice { file: 1, notice: 0 });
}

#[test]
fn truncate_caps_lines_and_appends_notice() {
    let mut file = parse_patch(PATCH).remove(0); // 2 hunks, 8 lines
    let untouched = file.clone();
    truncate_file_lines(&mut file, 10);
    assert_eq!(file, untouched, "under the cap: untouched");

    truncate_file_lines(&mut file, 6);
    let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    assert_eq!(lines, 6);
    assert_eq!(file.hunks.len(), 2);
    assert!(
        file_notices(&file)
            .iter()
            .any(|n| n.contains("first 6 of 8 lines"))
    );
    // body_height stays consistent with what actually renders.
    assert_eq!(
        body_height(&file),
        NOTICE_HEIGHT + 2.0 * HUNK_HEADER_HEIGHT + 6.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
    );

    // A cap below the first hunk's length drops later hunks entirely.
    let mut file = parse_patch(PATCH).remove(0);
    truncate_file_lines(&mut file, 3);
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(file.hunks[0].lines.len(), 3);
}

#[test]
fn gutters_fit_the_largest_line_number() {
    let files = parse_patch(PATCH);
    // src/main.rs second hunk ends at old 11 / new 12.
    assert_eq!(files[0].max_line, 12);
    assert_eq!(gutter_width(&files[0]), GUTTER_WIDTH);

    // Every digit count keeps ≥6px clear of the accent bar on the left
    // of the number (digits×6.6 + 8px right pad + 6px gap), and the
    // column never shrinks below the classic 36px.
    let mut file = files[0].clone();
    for digits in 1..=7u32 {
        file.max_line = 10u32.pow(digits) - 1;
        let w = gutter_width(&file);
        assert!(w >= GUTTER_WIDTH);
        let left_gap = w - (digits as f32 * 6.6 + 8.0);
        assert!(
            left_gap >= 6.0,
            "{digits} digits: left gap {left_gap} < 6px"
        );
    }
    // 4 digits outgrow the classic column now (the old formula left
    // them 1.6px off the bar — visually touching).
    file.max_line = 9999;
    assert!(gutter_width(&file) > GUTTER_WIDTH);
    file.max_line = 27404;
    assert!(
        gutter_width(&file)
            > gutter_width(&{
                let mut f = file.clone();
                f.max_line = 9999;
                f
            })
    );

    // Truncation refits the gutter to what actually renders: the first
    // 3 lines are ctx(1,1) / del(2,·) / add(·,2) — max line 2.
    let mut file = files[0].clone();
    truncate_file_lines(&mut file, 3);
    assert_eq!(file.max_line, 2);
}

#[test]
fn body_height_is_analytic() {
    let files = parse_patch(PATCH);
    let main = &files[0];
    let lines: usize = main.hunks.iter().map(|h| h.lines.len()).sum();
    assert_eq!(
        body_height(main),
        2.0 * HUNK_HEADER_HEIGHT + lines as f32 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
    );
    // Notices add height (added file: 1 notice + meta line inside hunk).
    let added = &files[1];
    assert_eq!(
        body_height(added),
        NOTICE_HEIGHT + HUNK_HEADER_HEIGHT + 3.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
    );
}

fn diff(checkout: &str, device: &str, cwd: &str, patch: &str) -> CheckoutDiff {
    CheckoutDiff {
        checkout_id: checkout.into(),
        device_id: device.into(),
        cwd: cwd.into(),
        patch: patch.into(),
        files: Vec::new(),
        additions: 0,
        deletions: 0,
        truncated: false,
        checksum: format!("sum-{}", patch.len()),
        updated_at: Utc::now(),
    }
}

fn chat(checkout: Option<&str>, device: &str, cwd: Option<&str>) -> Chat {
    Chat {
        id: "c1".into(),
        device_id: device.into(),
        cwd: cwd.map(Into::into),
        checkout_id: checkout.map(Into::into),
        created_at: Utc::now(),
        ..crate::test_fixtures::chat()
    }
}

#[test]
fn diff_resolution_prefers_checkout_id_then_cwd() {
    let diffs = vec![
        diff("co-1", "dev-a", "/repo/one", "x"),
        diff("co-2", "dev-b", "/repo/two", "y"),
    ];
    // checkout_id match wins even when cwd points elsewhere.
    let c = chat(Some("co-2"), "dev-a", Some("/repo/one"));
    assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-2");
    // Unknown checkout falls back to device+cwd.
    let c = chat(Some("co-9"), "dev-a", Some("/repo/one"));
    assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-1");
    // Wrong device still matches by cwd alone.
    let c = chat(None, "dev-z", Some("/repo/two"));
    assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-2");
    // Nothing to go on.
    let c = chat(None, "dev-a", None);
    assert!(resolve_diff(&diffs, &c).is_none());
    let c = chat(None, "dev-a", Some("/elsewhere"));
    assert!(resolve_diff(&diffs, &c).is_none());
}

#[test]
fn phases() {
    assert_eq!(diff_phase(None), DiffPhase::Preparing);
    let clean = diff("co", "d", "/w", "  \n");
    assert_eq!(diff_phase(Some(&clean)), DiffPhase::Clean);
    let full = diff("co", "d", "/w", "diff --git a/x b/x\n");
    assert_eq!(diff_phase(Some(&full)), DiffPhase::List);
    // Engine may report files without patch text (truncation edge).
    let mut summarized = diff("co", "d", "/w", "");
    summarized.files.push(cypher_proto::DiffFileSummary {
        path: "x".into(),
        old_path: None,
        status: "modified".into(),
        additions: 1,
        deletions: 0,
        binary: false,
    });
    assert_eq!(diff_phase(Some(&summarized)), DiffPhase::List);
}

#[test]
fn header_label_pluralizes() {
    assert_eq!(uncommitted_label(0), "0 Uncommitted changes");
    assert_eq!(uncommitted_label(1), "1 Uncommitted change");
    assert_eq!(uncommitted_label(4), "4 Uncommitted changes");
}

#[test]
fn scope_labels_and_clean_messages() {
    assert_eq!(
        scope_label(DiffScope::WorkingTree, 2, None),
        "2 Uncommitted changes"
    );
    assert_eq!(
        scope_label(DiffScope::Branch, 1, Some("main")),
        "1 Changed file vs main"
    );
    assert_eq!(scope_label(DiffScope::Branch, 3, None), "3 Changed files");
    assert_eq!(
        scope_label(DiffScope::LatestTurn, 2, None),
        "2 Changed files this turn"
    );
    assert_eq!(
        clean_message(DiffScope::WorkingTree, None),
        "No uncommitted changes"
    );
    assert_eq!(
        clean_message(DiffScope::Branch, Some("develop")),
        "No changes vs develop"
    );
    assert_eq!(
        clean_message(DiffScope::LatestTurn, None),
        "No changes this turn"
    );
}

#[test]
fn base_ref_defaults_to_repo_default_then_main() {
    let branches =
        |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
    // Engine order puts the repo default first — take it when it isn't
    // the checked-out branch itself.
    let b = branches(&["main", "feature"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("main")
    );
    // No origin/HEAD: engine "default" is the current branch — fall
    // through to main/master.
    let b = branches(&["feature", "main"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("main")
    );
    let b = branches(&["feature", "master"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("master")
    );
    // No main/master: any branch that isn't the current one.
    let b = branches(&["feature", "develop"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("develop")
    );
    // Checked out ON main: comparing main with itself is the honest
    // default (empty branch diff).
    let b = branches(&["main", "feature"]);
    assert_eq!(default_base_ref(&b, Some("main")).as_deref(), Some("main"));
    // Single-branch repo, and empty list.
    let b = branches(&["main"]);
    assert_eq!(default_base_ref(&b, Some("main")).as_deref(), Some("main"));
    assert_eq!(default_base_ref(&[], Some("main")), None);
}

#[test]
fn scope_modes_are_wire_stable() {
    // `mode` is the GetCheckoutDiff wire contract — engine matches on it.
    assert_eq!(DiffScope::WorkingTree.mode(), "workingTree");
    assert_eq!(DiffScope::Branch.mode(), "branch");
    assert_eq!(DiffScope::LatestTurn.mode(), "turn");
    assert_eq!(DiffScope::default(), DiffScope::WorkingTree);
}

#[test]
fn diff_frames_replace_lists_and_upsert_singles() {
    let mut diffs = Vec::new();
    let one = diff("co-1", "d", "/w", "p1");
    // Single frame inserts.
    assert!(apply_diff_frame(
        &mut diffs,
        serde_json::to_value(&one).unwrap()
    ));
    assert_eq!(diffs.len(), 1);
    // Identical frame is a no-op.
    assert!(!apply_diff_frame(
        &mut diffs,
        serde_json::to_value(&one).unwrap()
    ));
    // Same checkout upserts in place.
    let mut updated = one.clone();
    updated.patch = "p2".into();
    assert!(apply_diff_frame(
        &mut diffs,
        serde_json::to_value(&updated).unwrap()
    ));
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].patch, "p2");
    // List frame replaces wholesale.
    let two = diff("co-2", "d", "/x", "q");
    assert!(apply_diff_frame(
        &mut diffs,
        serde_json::to_value(vec![two.clone()]).unwrap()
    ));
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].checkout_id, "co-2");
    // Malformed frames change nothing.
    assert!(!apply_diff_frame(
        &mut diffs,
        serde_json::json!({"nope": true})
    ));
    assert_eq!(diffs[0].checkout_id, "co-2");
}

#[test]
fn full_diff_highlights_map_old_new_and_context_by_source_line() {
    let old_source = "fn old() {\n    let value = 1;\n}\n";
    let new_source = "fn new() {\n    let value = 2;\n}\n";
    let parse = |source| {
        Arc::new(
            cypher_syntax::highlight(cypher_syntax::HighlightRequest {
                source,
                path: Some("src/lib.rs"),
                fence_tag: None,
            })
            .unwrap(),
        )
    };
    let highlights = DiffHighlights {
        old: Some(parse(old_source)),
        new: Some(parse(new_source)),
    };
    let deleted = DiffLine {
        kind: LineKind::Del,
        old_no: Some(1),
        new_no: None,
        text: "fn old() {".into(),
    };
    let added = DiffLine {
        kind: LineKind::Add,
        old_no: None,
        new_no: Some(1),
        text: "fn new() {".into(),
    };
    let context = DiffLine {
        kind: LineKind::Context,
        old_no: Some(2),
        new_no: Some(2),
        text: "    let value = 2;".into(),
    };
    assert_eq!(
        highlights.source_ref(&deleted),
        Some(SourceLineRef {
            side: SourceSide::Old,
            line_number: 1
        })
    );
    assert_eq!(
        highlights.source_ref(&added),
        Some(SourceLineRef {
            side: SourceSide::New,
            line_number: 1
        })
    );
    assert_eq!(
        highlights.source_ref(&context),
        Some(SourceLineRef {
            side: SourceSide::New,
            line_number: 2
        })
    );
    assert!(
        highlights
            .spans(&deleted)
            .iter()
            .any(|span| span.kind == cypher_syntax::HighlightKind::Function)
    );
    assert!(
        highlights
            .spans(&added)
            .iter()
            .any(|span| span.kind == cypher_syntax::HighlightKind::Function)
    );
}

#[test]
fn split_context_uses_each_versions_lexical_state() {
    let parse = |source| {
        Arc::new(
            cypher_syntax::highlight(cypher_syntax::HighlightRequest {
                source,
                path: Some("x.rs"),
                fence_tag: None,
            })
            .unwrap(),
        )
    };
    let highlights = DiffHighlights {
        old: Some(parse("/*\nlet value = 1;\n*/\n")),
        new: Some(parse("// changed\nlet value = 1;\n// end\n")),
    };
    let line = DiffLine {
        kind: LineKind::Context,
        old_no: Some(2),
        new_no: Some(2),
        text: "let value = 1;".into(),
    };
    assert!(
        highlights
            .spans_for_side(&line, Side::Old)
            .iter()
            .any(|s| s.kind == cypher_syntax::HighlightKind::Comment)
    );
    assert!(
        highlights
            .spans_for_side(&line, Side::New)
            .iter()
            .any(|s| s.kind == cypher_syntax::HighlightKind::Keyword)
    );
    assert_ne!(
        highlights.spans_for_side(&line, Side::Old),
        highlights.spans_for_side(&line, Side::New)
    );
}

#[test]
fn excerpt_parses_old_and_new_hunks_as_separate_documents() {
    let file = FileDiff {
        path: "src/lib.rs".into(),
        old_path: None,
        status: FileStatus::Modified,
        binary: false,
        notices: vec![],
        hunks: vec![Hunk {
            header: "@@ -1,3 +1,3 @@".into(),
            lines: vec![
                DiffLine {
                    kind: LineKind::Context,
                    old_no: Some(1),
                    new_no: Some(1),
                    text: "/* start".into(),
                },
                DiffLine {
                    kind: LineKind::Del,
                    old_no: Some(2),
                    new_no: None,
                    text: "old body".into(),
                },
                DiffLine {
                    kind: LineKind::Add,
                    old_no: None,
                    new_no: Some(2),
                    text: "new body".into(),
                },
                DiffLine {
                    kind: LineKind::Context,
                    old_no: Some(3),
                    new_no: Some(3),
                    text: "end */".into(),
                },
            ],
        }],
        additions: 1,
        deletions: 1,
        max_line: 3,
    };
    let highlights = excerpt_highlights(&file, Lang::Rust).expect("excerpt");
    let deleted = &file.hunks[0].lines[1];
    let added = &file.hunks[0].lines[2];
    assert!(
        highlights
            .spans(deleted)
            .iter()
            .any(|span| span.kind == cypher_syntax::HighlightKind::Comment)
    );
    assert!(
        highlights
            .spans(added)
            .iter()
            .any(|span| span.kind == cypher_syntax::HighlightKind::Comment)
    );
}

#[test]
fn mismatched_full_sources_are_rejected_atomically() {
    let file = FileDiff {
        path: "src/lib.rs".into(),
        old_path: None,
        status: FileStatus::Modified,
        binary: false,
        notices: vec![],
        hunks: vec![Hunk {
            header: "@@ -1 +1 @@".into(),
            lines: vec![
                DiffLine {
                    kind: LineKind::Del,
                    old_no: Some(1),
                    new_no: None,
                    text: "let old = 1;".into(),
                },
                DiffLine {
                    kind: LineKind::Add,
                    old_no: None,
                    new_no: Some(1),
                    text: "let new = 2;".into(),
                },
            ],
        }],
        additions: 1,
        deletions: 1,
        max_line: 1,
    };
    let response = cypher_proto::CheckoutFileDiffText {
        diff_checksum: "sum".into(),
        old_text: Some("let old = 1;\n".into()),
        new_text: Some("different snapshot\n".into()),
        old_content_hash: None,
        new_content_hash: None,
        binary: false,
        truncated: false,
        stale: false,
    };
    assert!(!sources_match_patch(&file, &response));
    assert!(full_highlights(&file, Lang::Rust, &response).is_none());
}
