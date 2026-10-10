use super::*;
use cypher_doc::MessagePart;
use cypher_proto::{Chat, HarnessId};

// ---- transcript comments ----
// Quote normalization / preview moved to the shared `crate::comment_popup`
// module (they are exercised there); the transcript only wires the
// shared popup.

// ---- streaming parse wiring (the transcript side, not the parser) ----

#[test]
fn live_row_parse_work_is_bounded_per_commit() {
    // Drive the EXACT wiring `rows_for` uses (`parse_for_row`) with the
    // prefix-extending commit snapshots the doc watch delivers, and prove
    // the per-commit parse work stays O(reparsed tail): a full-reparse
    // wiring would feed ~N/2 × final_len bytes through the parser across N
    // commits; the incremental path stays within a small multiple of the
    // final length regardless of N.
    let mut live_parsers = HashMap::new();
    let mut tree_cache = HashMap::new();
    let paragraph = "A paragraph of streaming prose that keeps arriving.\n\n";
    let commits = 120usize;
    let mut text = String::new();
    let mut total_parsed = 0usize;
    for i in 0..commits {
        // Each commit appends ~half a paragraph (crosses block boundaries).
        let chunk = &paragraph[..paragraph.len() / 2];
        text.push_str(if i % 2 == 0 {
            chunk
        } else {
            &paragraph[paragraph.len() / 2..]
        });
        let (tree, outcome) =
            parse_for_row(true, "e1#p1", &text, &mut live_parsers, &mut tree_cache);
        assert!(!tree.blocks.is_empty());
        let ParseOutcome::Incremental {
            parsed_bytes,
            stable_prefix_blocks,
        } = outcome
        else {
            panic!("streaming commit must take the incremental path");
        };
        total_parsed += parsed_bytes;
        // Per commit: never a full reparse once the doc has grown past the
        // tail window (last two complete blocks + the partial trailing
        // one + the delta ≤ 3 paragraphs here).
        assert!(
            parsed_bytes <= 3 * paragraph.len(),
            "commit {i}: parsed {parsed_bytes} bytes — not bounded by the tail window"
        );
        // The stable prefix grows with the doc — settled blocks are never
        // re-touched (this is what keeps render caches valid).
        assert!(stable_prefix_blocks + 2 >= tree.blocks.len().saturating_sub(1));
    }
    // Across the whole stream: work is commits × O(tail), an order of
    // magnitude under the ~commits × len/2 a full-reparse wiring costs.
    let final_len = text.len();
    let full_reparse_cost = commits * final_len / 2;
    assert!(total_parsed <= commits * 3 * paragraph.len());
    assert!(
        total_parsed * 10 < full_reparse_cost,
        "total parsed {total_parsed} vs full-reparse ~{full_reparse_cost}"
    );

    // Live→complete handoff: the completed part adopts the live parser's
    // exact tree without parsing a single byte.
    let (_, outcome) = parse_for_row(false, "e1#p1", &text, &mut live_parsers, &mut tree_cache);
    assert_eq!(outcome, ParseOutcome::Handoff);
    // And the settled cache serves repeats with no work at all.
    let (_, outcome) = parse_for_row(false, "e1#p1", &text, &mut live_parsers, &mut tree_cache);
    assert_eq!(outcome, ParseOutcome::Cached);
}

// ---- stick-to-bottom spring ----

#[test]
fn spring_converges_to_a_fixed_target() {
    let mut spring = StickSpring::new();
    let target = 400.0;
    let mut pos = 0.0;
    let mut frames = 0;
    while pos < target && frames < 600 {
        pos = spring.step(pos, target, 1.0);
        frames += 1;
    }
    assert_eq!(pos, target, "spring must land exactly on the target");
    assert!(
        frames < 300,
        "400px should converge within 5s of frames, took {frames}"
    );
    // Once landed it stays landed (and idles out).
    for _ in 0..120 {
        pos = spring.step(pos, target, 1.0);
        assert_eq!(pos, target);
    }
    assert!(spring.is_idle(), "no residual motion at rest");
}

#[test]
fn spring_never_overshoots_or_oscillates() {
    let mut spring = StickSpring::new();
    let target = 250.0;
    let mut pos = 0.0;
    let mut last = pos;
    for _ in 0..600 {
        pos = spring.step(pos, target, 1.0);
        assert!(pos <= target, "overshoot: {pos} > {target}");
        assert!(
            pos >= last - 1e-3,
            "oscillation: position moved backwards {last} -> {pos}"
        );
        last = pos;
    }
    assert_eq!(pos, target);
}

#[test]
fn spring_feed_forward_tracks_constant_growth() {
    // Target grows 2px/frame (≈120px/s — a typical stream). After warmup
    // the EMA feed-forward must carry the viewport at the same rate with a
    // bounded, stable lag — a glide, not 0,0,0,Npx steps.
    let growth = 2.0;
    let mut spring = StickSpring::new();
    let mut target = 600.0;
    let mut pos = 600.0;
    let mut deltas: Vec<f32> = Vec::new();
    for frame in 0..400 {
        target += growth;
        let next = spring.step(pos, target, 1.0);
        if frame >= 200 {
            deltas.push(next - pos);
        }
        pos = next;
    }
    // Steady state: per-frame movement ≈ growth rate…
    let mean = deltas.iter().sum::<f32>() / deltas.len() as f32;
    assert!(
        (mean - growth).abs() < 0.2,
        "steady-state speed {mean} should track growth {growth}"
    );
    // …with no stepping (every frame moves, none jumps).
    for d in &deltas {
        assert!(*d > 0.0, "viewport stalled mid-stream");
        assert!(*d < growth * 3.0, "viewport jumped: {d}px in one frame");
    }
    // The EMA growth estimate itself has locked on.
    assert!((spring.target_vel() - growth).abs() < 0.3);
    // Lag stays bounded by the chase lead.
    assert!(target - pos <= SPRING_CHASE_MAX_LEAD + growth);
}

#[test]
fn spring_feed_forward_resets_when_target_shrinks() {
    let mut spring = StickSpring::new();
    let mut pos = 0.0;
    for i in 1..=50 {
        pos = spring.step(pos, 100.0 + i as f32 * 4.0, 1.0);
    }
    assert!(spring.target_vel() > 1.0);
    // A collapse (target shrinks by more than 1px) drops the estimate.
    spring.step(pos.min(120.0), 120.0, 1.0);
    assert_eq!(spring.target_vel(), 0.0);
}

#[test]
fn spring_catchup_frames_glide_instead_of_teleporting() {
    // A 5-frame hitch advances roughly as far as 5 single steps would —
    // sub-stepped, still clamped at the target.
    let target = 300.0;
    let mut a = StickSpring::new();
    let mut pos_a = 0.0;
    for _ in 0..5 {
        pos_a = a.step(pos_a, target, 1.0);
    }
    let mut b = StickSpring::new();
    let pos_b = b.step(0.0, target, 5.0);
    assert!((pos_a - pos_b).abs() < 1.0, "{pos_a} vs {pos_b}");
    assert!(pos_b <= target);
}

#[test]
fn restick_is_direction_aware() {
    // Scrolling away from the bottom never resticks, even inside the band
    // (a 20px wheel notch from the pinned bottom must break the pin).
    assert!(!Transcript::should_restick(20.0, 20.0));
    assert!(!Transcript::should_restick(69.0, 0.5));
    // Returning toward the bottom resticks once inside the 70px band…
    assert!(Transcript::should_restick(69.0, -20.0));
    assert!(Transcript::should_restick(0.0, -0.5));
    // …but not while still outside it.
    assert!(!Transcript::should_restick(200.0, -20.0));
    // No vertical movement — leave the pin alone.
    assert!(!Transcript::should_restick(50.0, 0.0));
}

#[test]
fn own_turn_reservation_is_a_min_height_for_the_turn() {
    let usable = 700.0;
    // A short turn reserves the rest of the usable viewport below it.
    assert_eq!(own_turn_reservation(usable, 100.0), 600.0);
    // Growth consumes the reservation 1:1 — total held height is stable.
    assert_eq!(own_turn_reservation(usable, 450.0), 250.0);
    // At/past the fill line nothing is reserved (bottom spring takes
    // over with no height jump).
    assert_eq!(own_turn_reservation(usable, 700.0), 0.0);
    assert_eq!(own_turn_reservation(usable, 1_200.0), 0.0);
}

fn parse(_: &str, text: &str) -> Arc<BlockTree> {
    Arc::new(parse_full(text))
}

#[test]
fn custom_spacing_preserves_the_streaming_to_settled_layout() {
    let live = assistant(
        "spacing",
        MessageStatus::Streaming,
        vec![text_part("t", "first\n\nsecond")],
    );
    let mut done = live.clone();
    done.status = Some(MessageStatus::Complete);
    let live_rows = rows_for_entry(&live, false, &mut parse);
    let done_rows = rows_for_entry(&done, false, &mut parse);
    assert_eq!(live_rows.len(), 2);
    for rows in [&live_rows, &done_rows] {
        assert_eq!(top_gap_for_style(None, &rows[0], 28.0, 20.0), 28.0);
        assert_eq!(
            top_gap_for_style(Some(&rows[0]), &rows[1], 28.0, 20.0),
            20.0
        );
    }
}

fn assistant(id: &str, status: MessageStatus, parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::Assistant,
        parts,
        status: Some(status),
        ..crate::test_fixtures::entry()
    }
}

fn text_part(id: &str, text: &str) -> MessagePart {
    MessagePart::Text {
        id: id.into(),
        text: text.into(),
        agent_text: None,
    }
}

fn tool_part(id: &str, command: &str) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call: ToolCall::Exec {
            command: command.into(),
        },
        is_error: false,
        resolved: true,
        output: None,
        progress: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
    }
}

const MD: &str = "# Title\n\npara one\n\n```rust\nlet x = 1;\n```";

#[test]
fn message_copy_icons_have_explicit_paint_colors() {
    for theme in [Theme::dark(), Theme::light()] {
        for copied in [false, true] {
            for enabled in [false, true] {
                let expected = if !enabled {
                    theme.text_muted.opacity(0.3)
                } else if copied {
                    theme.success
                } else {
                    theme.text_muted
                };
                let mut glyph = message_copy_icon(copied, enabled, &theme);
                assert_eq!(glyph.style().text.color, Some(expected));
                assert_eq!(glyph.style().size.width, Some(px(10.0).into()));
                assert_eq!(glyph.style().size.height, Some(px(10.0).into()));
            }
        }
    }
}

#[test]
fn message_copy_keeps_whole_markdown_and_skips_tool_internals() {
    let entry = assistant(
        "copy-full",
        MessageStatus::Complete,
        vec![
            text_part("t0", MD),
            tool_part("tool", "internal command should not be copied"),
            text_part("empty", ""),
            text_part("t1", "中文结尾 🐈\n\n  保留缩进和换行\n"),
            MessagePart::Input {
                id: "input".into(),
                request_id: "request".into(),
                questions: vec![],
                resolved: true,
            },
        ],
    );
    assert!(rows_for_entry(&entry, false, &mut parse).len() > 3);
    assert_eq!(
        message_copy_text(&entry).as_deref(),
        Some(format!("{MD}\n\n中文结尾 🐈\n\n  保留缩进和换行\n").as_str())
    );
}

#[test]
fn message_copy_user_matches_visible_mentions_and_attachments() {
    let raw = crate::attachments::with_attachments(
        "检查 [lib.rs](cypher-file:src/lib.rs)\n保留这一行",
        &["/private/attachment.png".into()],
    );
    let mut entry = assistant(
        "user-copy",
        MessageStatus::Complete,
        vec![text_part("t", &raw)],
    );
    entry.role = MessageRole::User;
    let copied = message_copy_text(&entry).unwrap();
    assert!(copied.contains("@lib.rs"));
    assert!(copied.ends_with("\n保留这一行"));
    assert!(!copied.contains("cypher-file:"));
    assert!(!copied.contains("/private/attachment.png"));
    assert!(!copied.contains("Attached images"));

    entry.parts = vec![text_part(
        "image-only",
        &crate::attachments::with_attachments("", &["/private/attachment.png".into()]),
    )];
    assert_eq!(message_copy_text(&entry), None);
}

#[test]
fn message_copy_handles_errors_and_empty_entries_without_internal_payloads() {
    let mut entry = assistant(
        "copy-empty",
        MessageStatus::Complete,
        vec![text_part("blank", " \n\t"), tool_part("tool", "internal")],
    );
    assert_eq!(message_copy_text(&entry), None);
    entry.role = MessageRole::System;
    entry.parts.push(MessagePart::Error {
        id: "error".into(),
        message: "Request failed\nDetailed reason".into(),
    });
    assert_eq!(
        message_copy_text(&entry).as_deref(),
        Some("Request failed\nDetailed reason")
    );
    entry.parts.clear();
    assert_eq!(message_copy_text(&entry), None);
}

#[test]
fn message_copy_lookup_uses_exact_entry_and_supports_pending_echoes() {
    let entries = vec![
        assistant(
            "first",
            MessageStatus::Complete,
            vec![text_part("t0", "first")],
        ),
        assistant(
            "selected",
            MessageStatus::Complete,
            vec![
                text_part("t1", "whole message"),
                text_part("t2", "continued response"),
            ],
        ),
    ];
    let echoes = vec![
        assistant(
            "selected",
            MessageStatus::Complete,
            vec![text_part("t", "stale mirror")],
        ),
        assistant(
            "pending",
            MessageStatus::Complete,
            vec![text_part("t", "pending text")],
        ),
    ];
    assert_eq!(
        message_for_copy(&entries, &echoes, "selected").and_then(message_copy_text),
        Some("whole message\n\ncontinued response".into())
    );
    assert_eq!(
        message_for_copy(&entries, &echoes, "pending").and_then(message_copy_text),
        Some("pending text".into())
    );
    assert!(message_for_copy(&entries, &echoes, "missing").is_none());
}

fn thought_part(id: &str, text: &str) -> MessagePart {
    MessagePart::Reasoning {
        id: id.into(),
        text: text.into(),
    }
}

#[test]
fn a_thought_folds_behind_a_collapsed_toggle() {
    let entry = assistant(
        "m1",
        MessageStatus::Complete,
        vec![
            thought_part("r0", "**Plan**\n\nRead the file first."),
            text_part("t1", "Done."),
        ],
    );
    let rows = rows_for_entry(&entry, false, &mut parse);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_ref()).collect();
    assert_eq!(ids, ["m1#r0.thought", "m1#r0.0", "m1#r0.1", "m1#t1.0"]);
    assert!(matches!(
        rows[0].kind,
        RowKind::Thought {
            blocks: 2,
            live: false,
            nested: false,
            ..
        }
    ));
    assert!(rows[0].turn_start);
    assert!(matches!(
        rows[1].kind,
        RowKind::ThoughtBlock { live: false, .. }
    ));

    // Collapsed by default: the toggle stands in for the thought.
    let mut folded = rows.clone();
    fold_closed_toggles(&mut folded, &Default::default());
    let ids: Vec<&str> = folded.iter().map(|r| r.id.as_ref()).collect();
    assert_eq!(ids, ["m1#r0.thought", "m1#t1.0"]);
    // Off the thought, the answer sits at the ordinary block gap.
    assert_eq!(top_gap_for(Some(&folded[0]), &folded[1]), GAP_BLOCK);

    let mut open = rows.clone();
    fold_closed_toggles(&mut open, &[("m1#r0.thought".into(), true)].into());
    assert_eq!(open.len(), rows.len());
}

#[test]
fn a_thought_is_live_only_while_it_is_the_streaming_tail() {
    let thinking = assistant(
        "m1",
        MessageStatus::Streaming,
        vec![thought_part("r0", "Hmm")],
    );
    let rows = rows_for_entry(&thinking, false, &mut parse);
    assert!(matches!(rows[0].kind, RowKind::Thought { live: true, .. }));
    assert!(is_live_markdown(&rows[1].kind));

    let answering = assistant(
        "m1",
        MessageStatus::Streaming,
        vec![thought_part("r0", "Hmm"), text_part("t1", "So")],
    );
    let rows = rows_for_entry(&answering, false, &mut parse);
    assert!(matches!(rows[0].kind, RowKind::Thought { live: false, .. }));
    assert!(!is_live_markdown(&rows[1].kind));
}

#[test]
fn a_reply_that_ended_thinking_keeps_its_timestamp_on_the_toggle() {
    let entry = assistant(
        "m1",
        MessageStatus::Aborted,
        vec![text_part("t0", "Partial"), thought_part("r1", "Then…")],
    );
    let rows = rows_for_entry(&entry, false, &mut parse);
    assert!(rows.last().unwrap().timestamp.is_some());
    let mut folded = rows.clone();
    fold_closed_toggles(&mut folded, &Default::default());
    let ids: Vec<&str> = folded.iter().map(|r| r.id.as_ref()).collect();
    assert_eq!(ids, ["m1#t0.0", "m1#r1.thought"]);
    assert!(folded[1].timestamp.is_some());
    assert_ne!(folded[1].version, rows[1].version);
}

fn ids(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|r| r.id.as_ref()).collect()
}

#[test]
fn thinking_between_tool_calls_folds_into_one_work_run() {
    let entry = assistant(
        "m1",
        MessageStatus::Complete,
        vec![
            thought_part("r0", "**Look around**\n\nList the files."),
            tool_part("x1", "ls"),
            tool_part("x2", "git status"),
            thought_part("r3", "Now build."),
            tool_part("x4", "cargo build"),
            thought_part("r5", "It built."),
            text_part("t6", "Done."),
        ],
    );
    let rows = rows_for_entry(&entry, false, &mut parse);
    assert_eq!(
        ids(&rows),
        [
            "m1#r0.activity",
            "m1#r0.thought",
            "m1#r0.0",
            "m1#r0.1",
            "m1#g0",
            "m1#r3.thought",
            "m1#r3.0",
            "m1#g1",
            "m1#r5.thought",
            "m1#r5.0",
            "m1#t6.0",
        ]
    );
    let RowKind::Activity {
        rows: covered,
        summary,
        auto_open,
    } = &rows[0].kind
    else {
        panic!("expected the work run's toggle");
    };
    assert_eq!(*covered, 9);
    assert_eq!(summary.as_ref(), "Ran 3 commands · 3 thoughts");
    assert!(!auto_open);
    assert!(rows[0].turn_start);
    assert!(rows[1..10].iter().all(|r| is_nested(&r.kind)));
    let RowKind::Thought { preview, .. } = &rows[1].kind else {
        panic!("expected a thought");
    };
    assert_eq!(preview.as_ref(), "Look around");

    // Closed, the whole run is one row above the answer.
    let mut folded = rows.clone();
    fold_closed_toggles(&mut folded, &Default::default());
    assert_eq!(ids(&folded), ["m1#r0.activity", "m1#t6.0"]);

    // Open, the run reads in order: chips, and thoughts still folded.
    let mut open = rows.clone();
    fold_closed_toggles(&mut open, &[("m1#r0.activity".into(), true)].into());
    assert_eq!(
        ids(&open),
        [
            "m1#r0.activity",
            "m1#r0.thought",
            "m1#g0",
            "m1#r3.thought",
            "m1#g1",
            "m1#r5.thought",
            "m1#t6.0",
        ]
    );
    // The rail runs on: no bare gap between the run's rows.
    assert_eq!(top_gap_for(Some(&open[0]), &open[1]), CHIPS_TOP_PAD);
    assert_eq!(top_gap_for(Some(&open[1]), &open[2]), 0.0);
    assert_eq!(top_gap_for(Some(&open[5]), &open[6]), GAP_BLOCK);
}

#[test]
fn the_mock_work_demo_settles_into_one_row() {
    // `CYPHER_MOCK_WORK` (`scripts/dev-demo.sh --work`): sixteen alternating
    // thought and command rows fold into one above the answer.
    let mut parts = Vec::new();
    for event in cypher_harness::mock::work_script() {
        cypher_doc::fold_event_into_parts(&mut parts, &event);
    }
    let answer = cypher_proto::AgentEvent::TextDelta {
        text: "Here is the pipeline.".into(),
    };
    cypher_doc::fold_event_into_parts(&mut parts, &answer);
    let entry = assistant("m1", MessageStatus::Complete, parts);
    let mut rows = rows_for_entry(&entry, false, &mut parse);
    fold_closed_toggles(&mut rows, &Default::default());
    assert_eq!(rows.len(), 2);
    let RowKind::Activity { summary, .. } = &rows[0].kind else {
        panic!("expected the work run's toggle");
    };
    assert_eq!(summary.as_ref(), "Ran 10 commands · 8 thoughts · 1 failed");
}

#[test]
fn the_tool_call_cap_folds_the_start_of_an_open_work_run() {
    let entry = assistant(
        "m1",
        MessageStatus::Complete,
        vec![
            thought_part("r0", "Look around."),
            tool_part("x1", "ls"),
            tool_part("x2", "git status"),
            tool_part("x3", "git log"),
            thought_part("r4", "Build it."),
            tool_part("x5", "cargo build"),
            tool_part("x6", "cargo test"),
            tool_part("x7", "cargo clippy"),
            thought_part("r8", "Ship it."),
            tool_part("x9", "git add ."),
            tool_part("x10", "git commit"),
            tool_part("x11", "git push"),
            text_part("t12", "Done."),
        ],
    );
    let mut open = rows_for_entry(&entry, false, &mut parse);
    fold_closed_toggles(&mut open, &[("m1#r0.activity".into(), true)].into());
    let none = Default::default();

    // Nine calls, five kept: the cut falls inside the second group, which
    // keeps its last two chips.
    let mut capped = open.clone();
    cap_work_runs(&mut capped, 5, &none);
    assert_eq!(
        ids(&capped),
        [
            "m1#r0.activity",
            "m1#r0.activity.overflow",
            "m1#g1",
            "m1#r8.thought",
            "m1#g2",
            "m1#t12.0",
        ]
    );
    let RowKind::RunOverflow {
        run,
        tools,
        thoughts,
    } = &capped[1].kind
    else {
        panic!("expected the run's overflow row");
    };
    assert_eq!((run.as_ref(), *tools, *thoughts), ("m1#r0.activity", 4, 2));
    assert_eq!(
        run_overflow_label(*tools, *thoughts),
        "Show 4 earlier tool calls and 2 thoughts"
    );
    assert!(matches!(capped[2].kind, RowKind::ToolGroup { skip: 1, .. }));
    assert_ne!(capped[2].version, open[4].version);
    // On the run's rail, like its chips.
    assert_eq!(top_gap_for(Some(&capped[0]), &capped[1]), CHIPS_TOP_PAD);
    assert_eq!(top_gap_for(Some(&capped[1]), &capped[2]), 0.0);

    // A cut between groups keeps the thinking that led into the first
    // kept call.
    let mut capped = open.clone();
    cap_work_runs(&mut capped, 6, &none);
    assert_eq!(
        ids(&capped),
        [
            "m1#r0.activity",
            "m1#r0.activity.overflow",
            "m1#r4.thought",
            "m1#g1",
            "m1#r8.thought",
            "m1#g2",
            "m1#t12.0",
        ]
    );
    assert!(matches!(capped[3].kind, RowKind::ToolGroup { skip: 0, .. }));

    // Revealed, the run shows whole under a row that folds it back.
    let mut revealed = open.clone();
    cap_work_runs(&mut revealed, 5, &["m1#r0.activity".into()].into());
    assert_eq!(revealed.len(), open.len() + 1);
    assert!(matches!(
        revealed[1].kind,
        RowKind::RunOverflow { tools: 0, .. }
    ));
    assert_eq!(&ids(&revealed)[2..], &ids(&open)[1..]);
    assert_eq!(run_overflow_label(0, 0), "Show fewer tool calls");

    // One call over the cap folds too, with the thinking before it.
    let mut capped = open.clone();
    cap_work_runs(&mut capped, 8, &none);
    assert_eq!(
        ids(&capped),
        [
            "m1#r0.activity",
            "m1#r0.activity.overflow",
            "m1#g0",
            "m1#r4.thought",
            "m1#g1",
            "m1#r8.thought",
            "m1#g2",
            "m1#t12.0",
        ]
    );
    assert!(matches!(capped[2].kind, RowKind::ToolGroup { skip: 1, .. }));
    assert!(matches!(
        capped[1].kind,
        RowKind::RunOverflow {
            tools: 1,
            thoughts: 1,
            ..
        }
    ));

    // "Show all", a cap the run fits, or a closed run: untouched.
    for limit in [0, 9, 10] {
        let mut uncapped = open.clone();
        cap_work_runs(&mut uncapped, limit, &none);
        assert_eq!(ids(&uncapped), ids(&open));
    }
    let mut closed = rows_for_entry(&entry, false, &mut parse);
    fold_closed_toggles(&mut closed, &Default::default());
    let before = ids(&closed).join(" ");
    cap_work_runs(&mut closed, 5, &none);
    assert_eq!(ids(&closed).join(" "), before);
}

#[test]
fn a_work_run_is_open_while_it_streams_and_closes_when_the_answer_starts() {
    let working = assistant(
        "m1",
        MessageStatus::Streaming,
        vec![tool_part("x0", "ls"), thought_part("r1", "Hmm")],
    );
    let rows = rows_for_entry(&working, false, &mut parse);
    assert!(matches!(
        rows[0].kind,
        RowKind::Activity {
            auto_open: true,
            ..
        }
    ));
    assert!(matches!(
        rows[2].kind,
        RowKind::Thought {
            live: true,
            nested: true,
            ..
        }
    ));
    let mut folded = rows.clone();
    fold_closed_toggles(&mut folded, &Default::default());
    assert_eq!(folded.len(), 3, "open run, thought still folded");
    // A click pins it closed.
    let mut closed = rows.clone();
    fold_closed_toggles(&mut closed, &[("m1#x0.activity".into(), false)].into());
    assert_eq!(ids(&closed), ["m1#x0.activity"]);

    let answering = assistant(
        "m1",
        MessageStatus::Streaming,
        vec![
            tool_part("x0", "ls"),
            thought_part("r1", "Hmm"),
            text_part("t2", "So"),
        ],
    );
    let mut rows = rows_for_entry(&answering, false, &mut parse);
    fold_closed_toggles(&mut rows, &Default::default());
    assert_eq!(ids(&rows), ["m1#x0.activity", "m1#t2.0"]);
}

#[test]
fn only_a_run_that_mixes_tools_and_thinking_folds() {
    // Tools alone: one group, as before.
    let entry = assistant(
        "m1",
        MessageStatus::Complete,
        vec![
            tool_part("x0", "ls"),
            text_part("t1", "   "),
            tool_part("x2", "pwd"),
            text_part("t3", "Done."),
        ],
    );
    let rows = rows_for_entry(&entry, false, &mut parse);
    assert_eq!(ids(&rows), ["m1#g0", "m1#t3.0"]);
    let RowKind::ToolGroup { tools, nested, .. } = &rows[0].kind else {
        panic!("expected a tool group");
    };
    assert_eq!(tools.len(), 2, "an empty text part splits nothing");
    assert!(!nested);

    // Answer text between a thought and the tools: separate runs.
    let entry = assistant(
        "m1",
        MessageStatus::Complete,
        vec![
            thought_part("r0", "Plan"),
            text_part("t1", "Let me look."),
            tool_part("x2", "ls"),
            text_part("t3", "Done."),
        ],
    );
    let mut rows = rows_for_entry(&entry, false, &mut parse);
    fold_closed_toggles(&mut rows, &Default::default());
    assert_eq!(ids(&rows), ["m1#r0.thought", "m1#t1.0", "m1#g0", "m1#t3.0"]);
}

#[test]
fn a_work_run_still_leads_to_the_worked_rule() {
    let mut entry = assistant(
        "m1",
        MessageStatus::Complete,
        vec![
            thought_part("r0", "Plan"),
            tool_part("x1", "ls"),
            text_part("t2", "Done."),
        ],
    );
    entry.completed_at = Some(90_000);
    let mut rows = rows_for_entry(&entry, false, &mut parse);
    fold_closed_toggles(&mut rows, &Default::default());
    assert_eq!(ids(&rows), ["m1#r0.activity", "m1#worked", "m1#t2.0"]);
}

#[test]
fn thought_previews_drop_title_markup() {
    assert_eq!(
        thought_preview("**Planning the fix**\n\nFirst…"),
        "Planning the fix"
    );
    assert_eq!(thought_preview("\n## Heading\nbody"), "Heading");
    assert_eq!(
        thought_preview("Check `__init__` first"),
        "Check `__init__` first"
    );
    assert_eq!(thought_preview("_Weighing options_"), "Weighing options");
    assert_eq!(thought_preview(""), "");
}

#[test]
fn thinking_alone_is_not_a_work_log() {
    let settled = |parts: Vec<MessagePart>| {
        let mut entry = assistant("m1", MessageStatus::Complete, parts);
        entry.completed_at = Some(90_000);
        rows_for_entry(&entry, false, &mut parse)
    };
    let worked = |rows: &[Row]| {
        rows.iter()
            .position(|r| matches!(r.kind, RowKind::Worked { .. }))
    };
    assert_eq!(
        worked(&settled(vec![
            thought_part("r0", "Hmm"),
            text_part("t1", "Hi")
        ])),
        None
    );
    // With real work, the rule follows the last thought, before the answer.
    let rows = settled(vec![
        thought_part("r0", "Hmm"),
        tool_part("tool-1", "ls"),
        thought_part("r2", "Now answer"),
        text_part("t3", "Hi"),
    ]);
    let at = worked(&rows).expect("a work rule");
    assert_eq!(rows[at - 1].id.as_ref(), "m1#r2.0");
    assert_eq!(rows[at + 1].id.as_ref(), "m1#t3.0");
}

#[test]
fn append_translation_folds_its_original_behind_a_toggle() {
    let original = "The answer.\n\n- one\n- two";
    let translated = "答案。\n\n- 一\n- 二";
    let part = MessagePart::Text {
        id: "t0".into(),
        text: format!("{original}\n\n---\n\n{translated}"),
        agent_text: Some(original.into()),
    };
    let entry = assistant("m1", MessageStatus::Complete, vec![part]);
    let rows = rows_for_entry(&entry, false, &mut parse);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_ref()).collect();
    // Toggle, the original's two blocks, the rule, the translation's two.
    assert_eq!(
        ids,
        [
            "m1#t0.original",
            "m1#t0.0",
            "m1#t0.1",
            "m1#t0.2",
            "m1#t0.3",
            "m1#t0.4"
        ]
    );
    assert!(matches!(
        rows[0].kind,
        RowKind::TranslationOriginal { blocks: 3 }
    ));
    assert!(rows[0].turn_start);

    // Collapsed by default: the translation keeps its block ids (quotes
    // map back through them), everything before it is folded away.
    let mut folded = rows.clone();
    fold_closed_toggles(&mut folded, &Default::default());
    let ids: Vec<&str> = folded.iter().map(|r| r.id.as_ref()).collect();
    assert_eq!(ids, ["m1#t0.original", "m1#t0.3", "m1#t0.4"]);
    assert!(folded.last().unwrap().timestamp.is_some());

    let mut open = rows.clone();
    fold_closed_toggles(&mut open, &[("m1#t0.original".into(), true)].into());
    assert_eq!(open.len(), rows.len());
}

#[test]
fn append_translation_shows_whole_until_the_translation_starts() {
    let original = "The answer.";
    // Replace mode, and an append rendering whose translation is empty.
    for text in ["答案。".to_string(), format!("{original}\n\n---\n\n")] {
        let part = MessagePart::Text {
            id: "t0".into(),
            text,
            agent_text: Some(original.into()),
        };
        let entry = assistant("m1", MessageStatus::Streaming, vec![part]);
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r.kind, RowKind::TranslationOriginal { .. }))
        );
    }
}

#[test]
fn live_entry_splits_per_block_with_id_continuity() {
    // Live rows split per block exactly like completed ones (the list
    // virtualizes them — the fading tail is the only per-frame work).
    let live = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", MD)]);
    let live_rows = rows_for_entry(&live, false, &mut parse);
    assert_eq!(live_rows.len(), 3, "one live row per top-level block");
    assert!(
        live_rows
            .iter()
            .all(|r| matches!(r.kind, RowKind::LiveMarkdown { .. }))
    );
    assert_eq!(live_rows[0].id.as_ref(), "m1#t0.0");
    assert_eq!(live_rows[2].id.as_ref(), "m1#t0.2");

    let done = assistant("m1", MessageStatus::Complete, vec![text_part("t0", MD)]);
    let done_rows = rows_for_entry(&done, false, &mut parse);
    assert_eq!(done_rows.len(), 3, "three top-level blocks");
    // Every block row keeps its id across the flip — no flicker on handoff.
    for (live, done) in live_rows.iter().zip(&done_rows) {
        assert_eq!(live.id, done.id);
        // The flip changes the version even at identical text (the
        // streaming bit), forcing a splice.
        assert_ne!(live.version, done.version);
    }
    assert!(matches!(
        done_rows[0].kind,
        RowKind::Markdown { block_ix: 0, .. }
    ));
}

#[test]
fn live_commit_changes_only_tail_row_versions() {
    // Streaming commit: appending to the last block leaves every settled
    // block row's (id, version) untouched — the diff splices only the tail.
    let t1 = "para one\n\npara two\n\npara three";
    let t2 = "para one\n\npara two\n\npara three grows here";
    let live1 = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", t1)]);
    let live2 = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", t2)]);
    let r1 = rows_for_entry(&live1, false, &mut parse);
    let r2 = rows_for_entry(&live2, false, &mut parse);
    assert_eq!(r1.len(), 3);
    assert_eq!(r2.len(), 3);
    assert_eq!(r1[0].version, r2[0].version, "settled block untouched");
    assert_eq!(r1[1].version, r2[1].version, "settled block untouched");
    assert_ne!(r1[2].version, r2[2].version, "tail block respliced");
    assert_eq!(diff_rows(&r1, &r2), Some((2..3, 1)));
}

#[test]
fn split_sibling_gaps_match_live_internal_spacing() {
    // The live row spaces its internal blocks by MD_BLOCK_GAP; after the
    // live→split handoff the same boundaries are inter-row gaps. They must
    // be identical or the whole message jumps at completion.
    let done = assistant(
        "m1",
        MessageStatus::Complete,
        vec![
            text_part("t0", MD),
            tool_part("a", "ls"),
            text_part("t1", "tail para"),
        ],
    );
    let rows = rows_for_entry(&done, false, &mut parse);
    // Rows: t0.0, t0.1, t0.2 (three MD blocks), g0, t1.0.
    assert_eq!(rows.len(), 5);
    // Sibling markdown blocks from the same part: md block gap.
    assert_eq!(
        top_gap_for(Some(&rows[0]), &rows[1]),
        markdown::render::MD_BLOCK_GAP
    );
    assert_eq!(
        top_gap_for(Some(&rows[1]), &rows[2]),
        markdown::render::MD_BLOCK_GAP
    );
    // Markdown → tool group and tool group → next part: block gap.
    assert_eq!(top_gap_for(Some(&rows[2]), &rows[3]), GAP_BLOCK);
    assert_eq!(top_gap_for(Some(&rows[3]), &rows[4]), GAP_BLOCK);
    // Turn starts get the turn gap regardless.
    assert_eq!(top_gap_for(None, &rows[0]), GAP_TURN);
}

#[test]
fn consecutive_tools_fold_into_groups_between_text() {
    let entry = assistant(
        "m2",
        MessageStatus::Complete,
        vec![
            text_part("t0", "before"),
            tool_part("a", "ls"),
            tool_part("b", "pwd"),
            text_part("t1", "after"),
            tool_part("c", "make"),
        ],
    );
    let rows = rows_for_entry(&entry, false, &mut parse);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_ref()).collect();
    assert_eq!(ids, ["m2#t0.0", "m2#g0", "m2#t1.0", "m2#g1"]);
    let RowKind::ToolGroup { tools, .. } = &rows[1].kind else {
        panic!("group expected")
    };
    assert_eq!(tools.len(), 2);
    assert!(rows[0].turn_start && !rows[1].turn_start);
}

#[test]
fn settled_turn_rules_off_its_work_before_the_answer() {
    let mut entry = assistant(
        "m4",
        MessageStatus::Complete,
        vec![
            text_part("t0", "before"),
            tool_part("a", "ls"),
            text_part("t1", "after"),
        ],
    );
    entry.created_at = 1_000;
    entry.completed_at = Some(93_400);
    let rows = rows_for_entry(&entry, false, &mut parse);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_ref()).collect();
    assert_eq!(ids, ["m4#t0.0", "m4#g0", "m4#worked", "m4#t1.0"]);
    let RowKind::Worked { label } = &rows[2].kind else {
        panic!("work rule expected before the answer")
    };
    assert_eq!(label.as_ref(), "Worked for 1m 32s");
    // Interior by construction: the entry's turn-start and timestamp
    // markers stay on real content rows.
    assert!(rows[0].turn_start && !rows[2].turn_start);
    assert!(rows[2].timestamp.is_none());
    assert!(rows[3].timestamp.is_some());
}

#[test]
fn work_rule_only_where_work_preceded_an_answer() {
    let span = |status, parts| {
        let mut entry = assistant("m5", status, parts);
        entry.created_at = 0;
        entry.completed_at = Some(5_000);
        entry
    };
    let has_rule = |entry: &SessionMessageEntry| {
        rows_for_entry(entry, false, &mut parse)
            .iter()
            .any(|r| matches!(r.kind, RowKind::Worked { .. }))
    };
    let worked = vec![
        text_part("t0", "before"),
        tool_part("a", "ls"),
        text_part("t1", "after"),
    ];
    assert!(has_rule(&span(MessageStatus::Complete, worked.clone())));
    // A live turn hasn't worked for anything yet.
    assert!(!has_rule(&span(MessageStatus::Streaming, worked.clone())));
    // Nothing to separate: a plain reply, and a turn that ended on a tool.
    assert!(!has_rule(&span(
        MessageStatus::Complete,
        vec![text_part("t0", "just an answer")]
    )));
    assert!(!has_rule(&span(
        MessageStatus::Complete,
        vec![text_part("t0", "before"), tool_part("a", "ls")]
    )));
    // Entries written before the stamp existed, and sub-second turns.
    let mut unstamped = span(MessageStatus::Complete, worked.clone());
    unstamped.completed_at = None;
    assert!(!has_rule(&unstamped));
    let mut brief = span(MessageStatus::Complete, worked);
    brief.completed_at = Some(400);
    assert!(!has_rule(&brief));
}

#[test]
fn worked_label_reads_the_entry_span() {
    assert_eq!(
        worked_label(1_000, Some(4_000)).as_deref(),
        Some("Worked for 3s")
    );
    assert_eq!(
        worked_label(0, Some(3_600_000)).as_deref(),
        Some("Worked for 60m 0s")
    );
    assert_eq!(worked_label(0, None), None);
    // A clock that ran backwards labels nothing.
    assert_eq!(worked_label(9_000, Some(1_000)), None);
}

#[test]
fn trailing_group_auto_opens_only_while_streaming() {
    let parts = vec![text_part("t0", "hi"), tool_part("a", "ls")];
    let streaming = assistant("m3", MessageStatus::Streaming, parts.clone());
    let rows = rows_for_entry(&streaming, false, &mut parse);
    let RowKind::ToolGroup { auto_open, .. } = rows[1].kind else {
        panic!()
    };
    assert!(auto_open, "trailing group opens while streaming");

    let complete = assistant("m3", MessageStatus::Complete, parts);
    let rows = rows_for_entry(&complete, false, &mut parse);
    let RowKind::ToolGroup { auto_open, .. } = rows[1].kind else {
        panic!()
    };
    assert!(!auto_open);

    // A non-trailing group never auto-opens.
    let mid = assistant(
        "m4",
        MessageStatus::Streaming,
        vec![tool_part("a", "ls"), text_part("t0", "hi")],
    );
    let rows = rows_for_entry(&mid, false, &mut parse);
    let RowKind::ToolGroup { auto_open, .. } = rows[0].kind else {
        panic!()
    };
    assert!(!auto_open);
}

// ---- in-chat find (⌘F) ----

/// Build the index the way [`Transcript::reindex_find`] does, from a list
/// of per-row counts.
fn find_state(counts: [u32; 5]) -> FindState {
    let mut state = FindState {
        query: "x".into(),
        counts: counts.to_vec(),
        ..FindState::default()
    };
    let mut running = 0;
    state.prefix.push(0);
    for count in counts {
        running += count;
        state.prefix.push(running);
    }
    state
}

#[test]
fn find_target_resolves_a_global_index_onto_the_row_that_holds_it() {
    // Empty rows on both sides of, and between, the rows with hits: the
    // search must never land on a row whose count is 0.
    let mut state = find_state([0, 3, 0, 2, 0]);
    assert_eq!(state.total(), 5);
    let targets: Vec<(usize, usize)> = (0..5)
        .map(|ix| {
            state.active = ix;
            state.target().expect("a target for every match")
        })
        .collect();
    assert_eq!(targets, [(1, 0), (1, 1), (1, 2), (3, 0), (3, 1)]);
    // An index past the end (rows shrank under a live reindex) clamps to
    // the last match instead of panicking or reporting a phantom row.
    state.active = 99;
    assert_eq!(state.target(), Some((3, 1)));
}

#[test]
fn find_target_is_none_without_matches() {
    let mut state = find_state([0; 5]);
    assert_eq!(state.total(), 0);
    assert_eq!(state.target(), None);
    state.active = 3;
    assert_eq!(state.target(), None);
}

#[test]
fn row_match_counts_cover_prompts_and_replies_but_not_chips() {
    // A user prompt: the bubble is one text element.
    let mut prompt = assistant("f1", MessageStatus::Complete, vec![]);
    prompt.role = MessageRole::User;
    prompt.status = None;
    prompt.parts = vec![text_part("t0", "Find the cypher, then CYPHER again")];
    let rows = rows_for_entry(&prompt, false, &mut parse);
    assert_eq!(row_match_count(&rows[0], "cypher"), 2);
    assert_eq!(row_match_count(&rows[0], "nothing"), 0);

    // A reply splits per markdown block, so each row counts its own.
    let reply = assistant(
        "f2",
        MessageStatus::Complete,
        vec![text_part(
            "t0",
            "cypher in prose\n\n```\nlet cypher = cypher();\n```",
        )],
    );
    let rows = rows_for_entry(&reply, false, &mut parse);
    assert_eq!(rows.len(), 2);
    assert_eq!(row_match_count(&rows[0], "cypher"), 1);
    assert_eq!(row_match_count(&rows[1], "cypher"), 2);

    // Tool groups render chips, not a laid-out text model — counting
    // them would promise a highlight nothing can paint.
    let tools = assistant(
        "f3",
        MessageStatus::Complete,
        vec![tool_part("a", "grep cypher")],
    );
    let rows = rows_for_entry(&tools, false, &mut parse);
    assert!(matches!(rows[0].kind, RowKind::ToolGroup { .. }));
    assert_eq!(row_match_count(&rows[0], "cypher"), 0);
}

#[test]
fn slash_command_rows_are_not_searchable() {
    // A settings command renders as a quiet action chip (plain text, no
    // selection model), so it must stay out of the index — exactly the
    // condition the renderer branches on.
    let mut entry = assistant("f4", MessageStatus::Complete, vec![]);
    entry.role = MessageRole::User;
    entry.status = None;
    entry.parts = vec![text_part("t0", "/model opus")];
    let rows = rows_for_entry(&entry, false, &mut parse);
    let RowKind::User {
        text,
        mentions,
        attachments,
        ..
    } = &rows[0].kind
    else {
        panic!("expected a user row");
    };
    assert!(renders_as_command_chip(text, mentions, attachments));
    assert_eq!(row_match_count(&rows[0], "model"), 0);
}

#[test]
fn steer_marks_user_rows_and_changes_their_version() {
    let mut entry = assistant("s1", MessageStatus::Complete, vec![]);
    entry.role = MessageRole::User;
    entry.status = None;
    entry.parts = vec![text_part("t0", "actually, use tabs")];
    let plain = rows_for_entry(&entry, false, &mut parse);
    let mut steered = plain.clone();
    mark_steer_rows(&mut steered);
    assert!(matches!(plain[0].kind, RowKind::User { steer: false, .. }));
    assert!(matches!(steered[0].kind, RowKind::User { steer: true, .. }));
    // The ledger can confirm a steer after the row rendered: the diff
    // must see a changed row.
    assert_ne!(plain[0].version, steered[0].version);
}

#[test]
fn user_rows_and_echo_versions() {
    let mut entry = assistant("u1", MessageStatus::Complete, vec![]);
    entry.role = MessageRole::User;
    entry.status = None;
    entry.parts = vec![text_part("t0", "hello")];
    let confirmed = rows_for_entry(&entry, false, &mut parse);
    let echoed = rows_for_entry(&entry, true, &mut parse);
    assert_eq!(confirmed.len(), 1);
    assert_eq!(confirmed[0].id, echoed[0].id);
    // Pending → confirmed changes the version so the row re-renders.
    assert_ne!(confirmed[0].version, echoed[0].version);
    assert!(matches!(
        &echoed[0].kind,
        RowKind::User { pending: true, .. }
    ));
}

#[test]
fn user_rows_split_attachment_refs_from_text() {
    let content = crate::attachments::with_attachments(
        "what color is this?",
        &["/data/uploads/ab12-red.png".to_string()],
    );
    let mut entry = assistant("u2", MessageStatus::Complete, vec![]);
    entry.role = MessageRole::User;
    entry.status = None;
    entry.parts = vec![text_part("t0", &content)];
    let rows = rows_for_entry(&entry, false, &mut parse);
    assert_eq!(rows.len(), 1);
    let RowKind::User {
        text, attachments, ..
    } = &rows[0].kind
    else {
        panic!("expected a user row");
    };
    assert_eq!(text.as_ref(), "what color is this?");
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].path, "/data/uploads/ab12-red.png");
    assert_eq!(attachments[0].name, "ab12-red.png");

    // Image-only send: no bubble text, refs parsed.
    let only = crate::attachments::with_attachments("", &["/a/p.png".to_string()]);
    entry.parts = vec![text_part("t0", &only)];
    let rows = rows_for_entry(&entry, false, &mut parse);
    let RowKind::User {
        text, attachments, ..
    } = &rows[0].kind
    else {
        panic!("expected a user row");
    };
    assert_eq!(text.as_ref(), "");
    assert_eq!(attachments.len(), 1);
}

/// A sent prompt's file mentions render as chips in the transcript: the
/// row carries the projected display text plus spans, while ordinary
/// prompts keep the empty-spans fast path. The row version derives from
/// the RAW text either way, so projection never perturbs the diff key.
/// A prompt's comments ride its user row — a comment-only send included
/// — and key its version, while an uncommented prompt keeps the plain one.
#[test]
fn user_rows_carry_their_comments() {
    let mut entry = assistant("u4", MessageStatus::Complete, vec![]);
    entry.role = MessageRole::User;
    entry.status = None;
    entry.parts = vec![text_part("t0", "")];
    let plain = rows_for_entry(&entry, false, &mut parse);
    assert_eq!(plain[0].version, 0);
    entry.comments = vec![MessageComment {
        quote: "the quote".into(),
        comment: "why?".into(),
    }];
    let rows = rows_for_entry(&entry, false, &mut parse);
    let RowKind::User { text, comments, .. } = &rows[0].kind else {
        panic!("expected a user row");
    };
    assert!(text.is_empty());
    assert_eq!(comments.as_slice(), entry.comments.as_slice());
    assert_ne!(rows[0].version, plain[0].version);
}

#[test]
fn user_rows_project_file_mentions_into_chips() {
    let raw = "look at [composer.rs](cypher-file:crates/ui/src/composer.rs) please";
    let mut entry = assistant("u3", MessageStatus::Complete, vec![]);
    entry.role = MessageRole::User;
    entry.status = None;
    entry.parts = vec![text_part("t0", raw)];
    let rows = rows_for_entry(&entry, false, &mut parse);
    let RowKind::User { text, mentions, .. } = &rows[0].kind else {
        panic!("expected a user row");
    };
    assert!(
        !text.contains("cypher-file:"),
        "raw link left visible: {text}"
    );
    assert!(text.contains("composer.rs"));
    assert_eq!(mentions.len(), 1);
    assert!(!mentions[0].is_dir);
    assert_eq!(mentions[0].path.as_ref(), "crates/ui/src/composer.rs");
    assert_eq!(&text[mentions[0].range.clone()], {
        let projected: &str = "\u{00A0}@composer.rs\u{00A0}";
        projected
    });
    assert_eq!(rows[0].version, (raw.len() as u64) << 1);

    entry.parts = vec![text_part("t0", "no mentions here")];
    let rows = rows_for_entry(&entry, false, &mut parse);
    let RowKind::User { text, mentions, .. } = &rows[0].kind else {
        panic!("expected a user row");
    };
    assert_eq!(text.as_ref(), "no mentions here");
    assert!(mentions.is_empty());
}

#[test]
fn diff_rows_appends_and_middle_edits() {
    let entry1 = assistant("m1", MessageStatus::Complete, vec![text_part("t0", "one")]);
    let entry2 = assistant("m2", MessageStatus::Complete, vec![text_part("t0", "two")]);
    let r1 = rows_for_entry(&entry1, false, &mut parse);
    let mut both = r1.clone();
    both.extend(rows_for_entry(&entry2, false, &mut parse));

    // Identical → None.
    assert!(diff_rows(&r1, &r1.clone()).is_none());
    // Append → splice at the tail.
    assert_eq!(diff_rows(&r1, &both), Some((1..1, 1)));
    // Removal from the end.
    assert_eq!(diff_rows(&both, &r1), Some((1..2, 0)));

    // Middle content change: only the changed row splices.
    let entry1b = assistant(
        "m1",
        MessageStatus::Complete,
        vec![text_part("t0", "one more")],
    );
    let mut both_b = rows_for_entry(&entry1b, false, &mut parse);
    both_b.extend(rows_for_entry(&entry2, false, &mut parse));
    assert_eq!(diff_rows(&both, &both_b), Some((0..1, 1)));

    // Full reset when everything shifts.
    let r2 = rows_for_entry(&entry2, false, &mut parse);
    assert_eq!(diff_rows(&r1, &r2), Some((0..1, 1)));
}

#[test]
fn diff_handles_live_to_split_growth() {
    let live = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", MD)]);
    let done = assistant("m1", MessageStatus::Complete, vec![text_part("t0", MD)]);
    let live_rows = rows_for_entry(&live, false, &mut parse);
    let done_rows = rows_for_entry(&done, false, &mut parse);
    // Same ids; every version flips its streaming bit → one 3-row splice.
    assert_eq!(diff_rows(&live_rows, &done_rows), Some((0..3, 3)));
}

#[test]
fn tool_diff_builds_real_hunks_with_context_and_numbers() {
    use crate::changes::LineKind;
    let old = (1..=20).map(|i| format!("line {i}")).collect::<Vec<_>>();
    let mut new = old.clone();
    new[9] = "LINE 10".into();
    let diff = cypher_proto::ToolDiff {
        path: "/w/a.rs".into(),
        old_text: Some(old.join("\n") + "\n"),
        new_text: new.join("\n") + "\n",
    };
    let Some(ToolDetail::Diff {
        file,
        old_text,
        new_text,
    }) = tool_detail(None, Some(&diff), None)
    else {
        panic!("expected diff detail");
    };
    // One hunk: the change plus 3 context lines each side, real numbers.
    assert_eq!(file.hunks.len(), 1);
    let hunk = &file.hunks[0];
    assert_eq!(hunk.header, "@@ -7,7 +7,7 @@");
    assert_eq!(hunk.lines.len(), 8); // 6 context + 1 del + 1 add
    let del = hunk
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Del)
        .expect("del line");
    assert_eq!(del.old_no, Some(10));
    assert_eq!(del.new_no, None);
    assert_eq!(del.text, "line 10");
    let add = hunk
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Add)
        .expect("add line");
    assert_eq!(add.new_no, Some(10));
    assert_eq!(add.text, "LINE 10");
    assert_eq!((file.additions, file.deletions), (1, 1));
    assert_eq!(old_text.as_deref(), diff.old_text.as_deref());
    assert_eq!(new_text.as_deref(), Some(diff.new_text.as_str()));
    // New files carry Added status (and no old numbers).
    let created = cypher_proto::ToolDiff {
        path: "/w/new.txt".into(),
        old_text: None,
        new_text: "only\n".into(),
    };
    let Some(ToolDetail::Diff {
        file,
        old_text,
        new_text,
    }) = tool_detail(None, Some(&created), None)
    else {
        panic!("expected diff detail");
    };
    assert_eq!(file.status, crate::changes::FileStatus::Added);
    assert!(old_text.is_none());
    assert_eq!(new_text.as_deref(), Some("only\n"));

    // Output: verbatim lines (indentation intact), counted-tail cap.
    let output = (0..40)
        .map(|i| format!("    indented {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let Some(ToolDetail::Output {
        lines,
        truncated_by,
    }) = tool_detail(Some(&output), None, None)
    else {
        panic!("expected output detail");
    };
    assert_eq!(lines.len(), OUTPUT_DETAIL_MAX_LINES);
    assert_eq!(truncated_by, 40 - OUTPUT_DETAIL_MAX_LINES);
    assert_eq!(lines[0].as_ref(), "    indented 0");

    // Nothing → no affordance.
    assert!(tool_detail(None, None, None).is_none());
    assert!(tool_detail(Some("\n\n"), None, None).is_none());
}

#[test]
fn tool_group_summaries() {
    let exec = |c: &str| ToolItem {
        call: ToolCall::Exec { command: c.into() },
        is_error: false,
        resolved: true,
        detail: None,
        invocation: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        depth: 0,
    };
    let edit = |p: &str| ToolItem {
        call: ToolCall::EditFile {
            path: p.into(),
            old_string: None,
            new_string: None,
        },
        is_error: false,
        resolved: true,
        detail: None,
        invocation: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        depth: 0,
    };
    let tools = vec![
        exec("ls"),
        exec("pwd"),
        exec("make"),
        edit("a.rs"),
        edit("b.rs"),
    ];
    assert_eq!(
        tool_group_summary(&tools),
        "Ran 3 commands · edited 2 files"
    );
    // Distinct-path dedupe: editing one file twice counts once.
    let tools = vec![edit("a.rs"), edit("a.rs")];
    assert_eq!(tool_group_summary(&tools), "Edited 1 file");
    // Failures append.
    let mut failing = exec("boom");
    failing.is_error = true;
    assert_eq!(tool_group_summary(&[failing]), "Ran 1 command · 1 failed");
    // Reads / searches / misc.
    let tools = vec![
        ToolItem {
            call: ToolCall::ReadFile { path: "x".into() },
            is_error: false,
            resolved: true,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            depth: 0,
        },
        ToolItem {
            call: ToolCall::Glob {
                pattern: "*.rs".into(),
            },
            is_error: false,
            resolved: true,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            depth: 0,
        },
        ToolItem {
            call: ToolCall::WebSearch { query: "q".into() },
            is_error: false,
            resolved: true,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            depth: 0,
        },
    ];
    assert_eq!(tool_group_summary(&tools), "Read 1 file · searched 2 times");
}

#[test]
fn tool_chip_labels_per_kind() {
    assert_eq!(
        tool_chip_content(&ToolCall::Exec {
            command: "cargo test".into()
        }),
        ("Run", "cargo test".to_string())
    );
    assert_eq!(
        tool_chip_content(&ToolCall::Search {
            pattern: "foo".into(),
            path: Some("src".into())
        }),
        ("Search", "foo in src".to_string())
    );
    assert_eq!(
        tool_chip_content(&ToolCall::ApplyPatch { path: None }),
        ("Patch", "workspace".to_string())
    );
    assert_eq!(
        tool_chip_content(&ToolCall::Mcp {
            server: "gh".into(),
            tool: "issues".into(),
            input: None
        }),
        ("MCP", "gh · issues".to_string())
    );
    let todo = ToolCall::Todo {
        items: vec![
            cypher_proto::TodoItem {
                text: "a".into(),
                done: true,
            },
            cypher_proto::TodoItem {
                text: "b".into(),
                done: false,
            },
        ],
    };
    assert_eq!(tool_chip_content(&todo), ("Todo", "1/2 done".to_string()));
}

#[test]
fn multiline_command_flattens_to_one_chip_line() {
    // The user's breaker: a multi-line script in a Run chip. The detail
    // must come out as ONE sanitized line — the chip's fixed 30px card
    // then truncates it with an ellipsis like the original's CSS.
    let (label, detail) = tool_chip_content(&ToolCall::Exec {
        command: "set -e\nfixture_in_original=0\n\tgrep -c  \"x\"".into(),
    });
    assert_eq!(label, "Run");
    assert_eq!(detail, "set -e fixture_in_original=0 grep -c \"x\"");
    assert!(!detail.contains('\n'));
    // The chip row height is a constant, independent of content shape.
    assert_eq!(chips_height(1), CHIPS_TOP_PAD + CHIP_HEIGHT);
    // Every detail kind is sanitized (MCP inputs / queries are model text).
    let (_, q) = tool_chip_content(&ToolCall::WebSearch {
        query: "line one\nline two".into(),
    });
    assert_eq!(q, "line one line two");
}

#[test]
fn tool_rows_show_name_parameter_and_status() {
    let tool = ToolItem {
        call: ToolCall::Unknown {
            name: "apply_patch".into(),
            input: None,
        },
        is_error: false,
        resolved: true,
        detail: Some(Arc::new(ToolDetail::Output {
            lines: vec!["Applied patch:".into(), "Added src/new.rs".into()],
            truncated_by: 0,
        })),
        invocation: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        depth: 0,
    };
    let text = |tool: &ToolItem| {
        let (label, parameter) = tool_row_text(tool);
        (label, parameter, ToolStatus::of(tool))
    };
    // A generic tool's name is its label; nothing repeats it.
    assert_eq!(
        text(&tool),
        ("apply_patch".into(), String::new(), ToolStatus::Completed)
    );

    // A known kind says its name once, then what it acted on.
    let no_output = ToolItem {
        call: ToolCall::ReadFile {
            path: "README.md".into(),
        },
        detail: None,
        ..tool
    };
    assert_eq!(
        text(&no_output),
        ("Read".into(), "README.md".into(), ToolStatus::Completed)
    );

    let failed = ToolItem {
        is_error: true,
        ..no_output.clone()
    };
    assert_eq!(text(&failed).2, ToolStatus::Failed);
    // A failed call that also never resolved still reads as failed.
    assert_eq!(
        ToolStatus::of(&ToolItem {
            resolved: false,
            ..failed
        }),
        ToolStatus::Failed
    );

    let running = ToolItem {
        resolved: false,
        ..no_output
    };
    assert_eq!(text(&running).2, ToolStatus::Running);

    let mcp = ToolItem {
        call: ToolCall::Mcp {
            server: "demo-notes".into(),
            tool: "search_notes".into(),
            input: None,
        },
        ..running.clone()
    };
    assert_eq!(
        text(&mcp),
        (
            "MCP".into(),
            "demo-notes · search_notes".into(),
            ToolStatus::Running
        )
    );

    // A script the doc did not keep has no parameter at all.
    let script = ToolItem {
        call: ToolCall::Unknown {
            name: "codemode".into(),
            input: None,
        },
        ..running
    };
    assert_eq!(
        text(&script),
        ("Script".into(), String::new(), ToolStatus::Running)
    );
}

fn script_part(id: &str, code: &str) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call: ToolCall::Unknown {
            name: "codemode".into(),
            input: Some(serde_json::json!({ "code": code })),
        },
        is_error: false,
        resolved: true,
        output: Some("version 1.0.0.2".into()),
        progress: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
    }
}

fn group_tools(rows: &[Row]) -> Vec<(String, u8)> {
    rows.iter()
        .find_map(|row| match &row.kind {
            RowKind::ToolGroup { tools, .. } => Some(
                tools
                    .iter()
                    .map(|tool| (tool_chip_content(&tool.call).1, tool.depth))
                    .collect(),
            ),
            _ => None,
        })
        .expect("a tool group")
}

#[test]
fn script_calls_nest_under_their_script() {
    // Pi ran a script (`s`) next to a direct call (`d`); the script's own
    // calls (`s/1`, `s/2`) arrived after `d` and still list under `s`.
    let entry = assistant(
        "m",
        MessageStatus::Complete,
        vec![
            script_part("s", "await tools.bash({ command: 'ls' })"),
            tool_part("d", "pwd"),
            tool_part("s/1", "ls"),
            tool_part("s/2", "cat release.json"),
        ],
    );
    let rows = rows_for_entry(&entry, false, &mut parse);
    assert_eq!(
        group_tools(&rows),
        vec![
            ("bash".to_string(), 0),
            ("ls".to_string(), 1),
            ("cat release.json".to_string(), 1),
            ("pwd".to_string(), 0),
        ]
    );
    let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
        panic!("tool group first");
    };
    assert_eq!(tool_group_summary(tools), "Ran 3 commands and 1 script");

    // Nesting only follows ids: a slash with no caller in the group, or
    // nothing nested at all, keeps arrival order at depth 0.
    let entry = assistant(
        "m2",
        MessageStatus::Complete,
        vec![tool_part("a", "one"), tool_part("x/1", "two")],
    );
    let rows = rows_for_entry(&entry, false, &mut parse);
    assert_eq!(
        group_tools(&rows),
        vec![("one".to_string(), 0), ("two".to_string(), 0)]
    );
}

#[test]
fn nested_calls_keep_sibling_order_and_go_deeper() {
    let item = |command: &str| ToolItem {
        call: ToolCall::Exec {
            command: command.into(),
        },
        is_error: false,
        resolved: true,
        detail: None,
        invocation: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        depth: 0,
    };
    let nested = nest_tool_calls(vec![
        ("a".into(), item("a")),
        ("a/1".into(), item("a/1")),
        ("b".into(), item("b")),
        ("a/1/1".into(), item("a/1/1")),
        ("a/2".into(), item("a/2")),
    ]);
    let order: Vec<(String, u8)> = nested
        .iter()
        .map(|tool| (tool_chip_content(&tool.call).1, tool.depth))
        .collect();
    assert_eq!(
        order,
        vec![
            ("a".to_string(), 0),
            ("a/1".to_string(), 1),
            ("a/1/1".to_string(), 2),
            ("a/2".to_string(), 1),
            ("b".to_string(), 0),
        ]
    );
    // Depth is part of the row's identity: re-nesting re-splices.
    let flat: Vec<ToolItem> = nested
        .iter()
        .cloned()
        .map(|tool| ToolItem { depth: 0, ..tool })
        .collect();
    assert_ne!(
        tool_fingerprint(&nested, false),
        tool_fingerprint(&flat, false)
    );
}

#[test]
fn script_invocation_is_the_code_itself() {
    let code = "\n\nconst a = await tools.read({ path: \"x\" });\nreturn a.length;\n";
    let Some(ToolDetail::Output {
        lines,
        truncated_by,
    }) = call_block(&ToolCall::Unknown {
        name: "codemode".into(),
        input: Some(serde_json::json!({ "code": code })),
    })
    else {
        panic!("expected an output block")
    };
    assert_eq!(truncated_by, 0);
    assert_eq!(
        lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
        vec![
            "const a = await tools.read({ path: \"x\" });",
            "return a.length;"
        ]
    );
    // Scripts get more lines than a command echo before the tail.
    let long = (0..100)
        .map(|i| format!("text({i});"))
        .collect::<Vec<_>>()
        .join("\n");
    let Some(ToolDetail::Output {
        lines,
        truncated_by,
    }) = call_block(&ToolCall::Unknown {
        name: "codemode".into(),
        input: Some(serde_json::json!({ "code": long })),
    })
    else {
        panic!("expected an output block")
    };
    assert_eq!(lines.len(), SCRIPT_DETAIL_MAX_LINES);
    assert_eq!(truncated_by, 100 - SCRIPT_DETAIL_MAX_LINES);
    // tool_search shows its query.
    let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Unknown {
        name: "tool_search".into(),
        input: Some(serde_json::json!({ "query": "discord" })),
    }) else {
        panic!("expected an output block")
    };
    assert_eq!(lines[0].as_ref(), "discord");
}

#[test]
fn call_block_carries_the_full_invocation() {
    // Multi-line command: verbatim lines, not the flattened chip line.
    let Some(ToolDetail::Output {
        lines,
        truncated_by,
    }) = call_block(&ToolCall::Exec {
        command: "set -e\ncargo test".into(),
    })
    else {
        panic!("expected an output block")
    };
    assert_eq!(truncated_by, 0);
    assert_eq!(
        lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
        vec!["set -e", "cargo test"]
    );

    // A long single-line command soft-wraps instead of ellipsizing.
    let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Exec {
        command: "x".repeat(CALL_WRAP_COLS * 2 + 10),
    }) else {
        panic!("expected an output block")
    };
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|l| l.chars().count() <= CALL_WRAP_COLS));

    // MCP input pretty-prints under the `server · tool` line.
    let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Mcp {
        server: "gh".into(),
        tool: "issues".into(),
        input: Some(serde_json::json!({"repo": "cypher"})),
    }) else {
        panic!("expected an output block")
    };
    assert_eq!(lines[0].as_ref(), "gh · issues");
    assert!(lines.iter().any(|l| l.contains("\"repo\": \"cypher\"")));

    // A sanitized extension call explains why its full input is absent
    // instead of expanding to a duplicate of the tool name.
    let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Unknown {
        name: "apply_patch".into(),
        input: None,
    }) else {
        panic!("expected an output block")
    };
    assert_eq!(
        lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
        vec!["apply_patch", "Input details not retained in chat"]
    );

    // Todos list one item per line with checkbox state.
    let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Todo {
        items: vec![
            cypher_proto::TodoItem {
                text: "a".into(),
                done: true,
            },
            cypher_proto::TodoItem {
                text: "b".into(),
                done: false,
            },
        ],
    }) else {
        panic!("expected an output block")
    };
    assert_eq!(
        lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
        vec!["[x] a", "[ ] b"]
    );

    // Blank invocation → no block; the chip stays a plain card.
    assert!(
        call_block(&ToolCall::Exec {
            command: "  \n ".into()
        })
        .is_none()
    );
}

#[test]
fn timestamp_strip_lands_on_the_last_settled_row() {
    use chrono::FixedOffset;
    // Fixed zone (UTC−4): "Jul 1, 3:45 PM" — the exact formatTimestamp
    // shape (short month, numeric day, no leading zero, 2-digit minutes).
    let tz = FixedOffset::west_opt(4 * 3600).unwrap();
    let ms = chrono::DateTime::parse_from_rfc3339("2026-07-01T19:45:00Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(format_timestamp(ms, &tz), "Jul 1, 3:45 PM");

    // User entries carry the strip on their single row (pending too).
    let user = SessionMessageEntry {
        id: "u1".into(),
        role: MessageRole::User,
        parts: vec![text_part("p1", "hi")],
        created_at: ms,
        ..crate::test_fixtures::entry()
    };
    let rows = rows_for_entry(&user, true, &mut parse);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].timestamp, Some(ms));

    // Assistant entries: strip on the LAST row once settled…
    let done = assistant(
        "a1",
        MessageStatus::Complete,
        vec![text_part("p1", "one\n\ntwo")],
    );
    let rows = rows_for_entry(&done, false, &mut parse);
    assert!(rows.len() >= 2);
    assert_eq!(rows.last().unwrap().timestamp, Some(done.created_at));
    assert!(rows[..rows.len() - 1].iter().all(|r| r.timestamp.is_none()));

    // …but never mid-stream (chat-view.tsx: no hover under a moving reply).
    let live = assistant(
        "a2",
        MessageStatus::Streaming,
        vec![text_part("p1", "streaming…")],
    );
    let rows = rows_for_entry(&live, false, &mut parse);
    assert!(rows.iter().all(|r| r.timestamp.is_none()));
    // Every row knows its entry (the hover group).
    assert!(rows.iter().all(|r| r.entry_id.as_ref() == live.id));
}

fn answered(model: &str, requested: Option<&str>) -> AnsweredModel {
    AnsweredModel {
        model: model.into(),
        requested: requested.map(str::to_owned),
    }
}

/// The answering models ride the settled answer's strip, and only a
/// different model — not a dated snapshot of the requested one — flags it.
#[test]
fn the_answering_model_labels_the_settled_strip() {
    let mut done = assistant(
        "a1",
        MessageStatus::Complete,
        vec![text_part("p1", "one\n\ntwo")],
    );
    let plain = rows_for_entry(&done, false, &mut parse);
    assert!(plain.iter().all(|r| r.answered.is_none()), "none recorded");

    done.models = vec![
        answered("gpt-6-astra-2026-09-01", Some("gpt-6-astra")),
        answered("gpt-6-astra", None),
    ];
    let rows = rows_for_entry(&done, false, &mut parse);
    let last = rows.last().unwrap();
    assert_eq!(
        last.answered,
        Some(AnsweredLabel {
            text: "gpt-6-astra-2026-09-01, gpt-6-astra".into(),
            substituted: None,
        })
    );
    assert!(rows[..rows.len() - 1].iter().all(|r| r.answered.is_none()));
    // The diff key changes with the label, or the strip would not repaint.
    assert_ne!(last.version, plain.last().unwrap().version);

    done.models = vec![
        answered("gpt-6-astra", None),
        answered("gpt-5.4-mini", Some("gpt-6-astra")),
    ];
    let rows = rows_for_entry(&done, false, &mut parse);
    assert_eq!(
        rows.last().unwrap().answered,
        Some(AnsweredLabel {
            text: "gpt-6-astra, gpt-5.4-mini".into(),
            substituted: Some("Requested gpt-6-astra, answered by gpt-5.4-mini".into()),
        })
    );

    // Not while the turn is still streaming.
    done.status = Some(MessageStatus::Streaming);
    let rows = rows_for_entry(&done, false, &mut parse);
    assert!(rows.iter().all(|r| r.answered.is_none()));
}

#[test]
fn single_line_collapses_all_whitespace_runs() {
    assert_eq!(single_line("a\nb"), "a b");
    assert_eq!(single_line("  a\t\t b \r\n c  "), "a b c");
    assert_eq!(single_line("plain"), "plain");
    assert_eq!(single_line(""), "");
    assert_eq!(single_line("\n\n"), "");
}

#[test]
fn tool_group_cap_keeps_the_last_calls() {
    // Under the cap nothing folds; over it, the LAST `limit` chips stay.
    assert_eq!(hidden_tool_count(5, 5, false), 0);
    assert_eq!(hidden_tool_count(12, 5, false), 7);
    // Strict: one call over the cap folds too.
    assert_eq!(hidden_tool_count(6, 5, false), 1);
    assert_eq!(hidden_tool_count(7, 5, false), 2);
    // Revealed rows and the "show all" setting hide nothing.
    assert_eq!(hidden_tool_count(12, 5, true), 0);
    assert_eq!(hidden_tool_count(12, 0, false), 0);
    // A capped group is bounded in height no matter how long the run is.
    let capped = chips_height(12 - hidden_tool_count(12, 5, false)) + OVERFLOW_ROW_HEIGHT;
    assert_eq!(capped, chips_height(5) + OVERFLOW_ROW_HEIGHT);
    assert!(capped < chips_height(12));
}

#[test]
fn chips_height_is_analytic() {
    assert_eq!(chips_height(0), 0.0);
    assert_eq!(chips_height(1), CHIPS_TOP_PAD + CHIP_HEIGHT);
    assert_eq!(
        chips_height(3),
        CHIPS_TOP_PAD + 3.0 * CHIP_HEIGHT + 2.0 * CHIP_GAP
    );
}

#[test]
fn flavour_words_rotate_every_seven_seconds() {
    let seed = flavour_seed("chat-1");
    assert_eq!(flavour_word(seed, 0), flavour_word(seed, 6));
    assert_ne!(flavour_word(seed, 0), flavour_word(seed, 7));
    // Deterministic per chat; different chats usually differ in phase.
    assert_eq!(flavour_word(seed, 3), flavour_word(seed, 3));
    assert_eq!(format_elapsed(59), "59s");
    assert_eq!(format_elapsed(92), "1m 32s");
    assert_eq!(format_elapsed(-5), "0s");
}

#[test]
fn throughput_label_drops_a_stale_rate_but_keeps_the_count() {
    let now = chrono::Utc::now();
    let reading = |rate, tokens, age_ms| cypher_proto::Throughput {
        tokens_per_second: rate,
        average_tokens_per_second: None,
        output_tokens: tokens,
        sampled_at: now - chrono::Duration::milliseconds(age_ms),
    };
    assert_eq!(
        throughput_label(&reading(Some(52), 3_400, 400), now).as_deref(),
        Some("↓ 3.4k tokens · 52 tok/s")
    );
    // A stalled stream: the last rate no longer describes anything.
    assert_eq!(
        throughput_label(&reading(Some(52), 3_400, 5_000), now).as_deref(),
        Some("↓ 3.4k tokens")
    );
    // Between messages (a tool running) the host sends no rate at all.
    assert_eq!(
        throughput_label(&reading(None, 812, 100), now).as_deref(),
        Some("↓ 812 tokens")
    );
    assert_eq!(throughput_label(&reading(None, 0, 100), now), None);
}

#[test]
fn throughput_label_falls_back_to_the_last_message_average() {
    let now = chrono::Utc::now();
    let reading = |rate, average, age_ms| cypher_proto::Throughput {
        tokens_per_second: rate,
        average_tokens_per_second: average,
        output_tokens: 20_400,
        sampled_at: now - chrono::Duration::milliseconds(age_ms),
    };
    // A fresh live rate wins.
    assert_eq!(
        throughput_label(&reading(Some(90), Some(60), 400), now).as_deref(),
        Some("↓ 20k tokens · 90 tok/s")
    );
    // Between messages, and on another device whose copy is 20s old.
    assert_eq!(
        throughput_label(&reading(None, Some(60), 100), now).as_deref(),
        Some("↓ 20k tokens · 60 tok/s")
    );
    assert_eq!(
        throughput_label(&reading(Some(90), Some(60), 20_000), now).as_deref(),
        Some("↓ 20k tokens · 60 tok/s")
    );
}

#[test]
fn sending_bridge_holds_until_the_turn_outdates_the_send() {
    let send = chrono::DateTime::parse_from_rfc3339("2026-08-13T10:00:00Z")
        .unwrap()
        .to_utc();
    let before = send - chrono::Duration::seconds(90);
    let after = send + chrono::Duration::seconds(2);
    // In flight, row still on the previous turn (or no row yet).
    assert!(sending_bridge(Some(send), Some(before)));
    assert!(sending_bridge(Some(send), None));
    // The turn started after the send fired — timer takes over.
    assert!(!sending_bridge(Some(send), Some(after)));
    // No send in flight: never a bridge, whatever the row says.
    assert!(!sending_bridge(None, Some(before)));
    assert!(!sending_bridge(None, None));
}

#[test]
fn empty_text_parts_produce_no_rows() {
    let entry = assistant(
        "m9",
        MessageStatus::Streaming,
        vec![text_part("t0", ""), text_part("t1", "   ")],
    );
    assert!(rows_for_entry(&entry, false, &mut parse).is_empty());
}

// ---- Session Fork v1 (affordance gating + tooltips) ----

fn pi_chat(child: bool, non_pi: bool) -> Chat {
    Chat {
        id: "chat-1".into(),
        device_id: "dev-1".into(),
        title: Some("My chat".into()),
        config: Some(cypher_proto::ChatConfig {
            harness: if non_pi {
                HarnessId::Mock
            } else {
                HarnessId::Pi
            },
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
        }),
        created_at: chrono::Utc::now(),
        child: child.then(|| cypher_proto::ChildChat {
            parent_chat_id: "p".into(),
            parent_run_id: "r".into(),
            agent: "a".into(),
            task: "t".into(),
            mode: cypher_proto::SubagentRunMode::Sync,
            tool_call_id: None,
            profile: cypher_proto::ChildAgentProfile {
                system_prompt: String::new(),
                tools: vec![],
                model: None,
                thinking: None,
            },
        }),
        ..crate::test_fixtures::chat()
    }
}

#[test]
fn fork_gate_enables_only_settled_local_pi_root_chats() {
    // A plain Pi root chat whose HOST is online is forkable (live/offline
    // are the remaining gates).
    assert_eq!(
        fork_gate(false, Some(&pi_chat(false, false)), false, false, true),
        ForkGate::Enabled
    );
    // A remote source chat whose HOST DEVICE is offline is refused.
    assert_eq!(
        fork_gate(false, Some(&pi_chat(false, false)), false, false, false),
        ForkGate::Disabled("The device hosting this chat is offline.")
    );
    // Embedded (temporary Side Chat) panels are always inert.
    assert_eq!(
        fork_gate(true, Some(&pi_chat(false, false)), false, false, true),
        ForkGate::Disabled("Side chats can't be forked.")
    );
    // Non-Pi config is refused.
    assert_eq!(
        fork_gate(false, Some(&pi_chat(false, true)), false, false, true),
        ForkGate::Disabled("Only Pi chats can be forked.")
    );
    // Child subagent chats are refused.
    assert_eq!(
        fork_gate(false, Some(&pi_chat(true, false)), false, false, true),
        ForkGate::Disabled("Subagent chats can't be forked.")
    );
    // A live (Working/Awaiting) run disables the affordance.
    assert_eq!(
        fork_gate(false, Some(&pi_chat(false, false)), true, false, true),
        ForkGate::Disabled("Wait for the chat to finish before forking.")
    );
    // Offline engine disables everything (local stays authoritative).
    assert_eq!(
        fork_gate(false, Some(&pi_chat(false, false)), false, true, true),
        ForkGate::Disabled("The engine is offline.")
    );
    // No selected chat row disables too.
    assert_eq!(
        fork_gate(false, None, false, false, true),
        ForkGate::Disabled("No chat selected.")
    );
}

#[test]
fn fork_tooltips_are_role_specific_and_honour_disabled_reasons() {
    // Enabled tooltip text is EXACTLY these strings.
    assert_eq!(
        fork_tooltip(MessageRole::User, &ForkGate::Enabled),
        "Fork before this message"
    );
    assert_eq!(
        fork_tooltip(MessageRole::Assistant, &ForkGate::Enabled),
        "Fork after this response"
    );
    // System rows never emit an affordance, but a fallback stays coherent.
    assert_eq!(
        fork_tooltip(MessageRole::System, &ForkGate::Enabled),
        "Fork after this message"
    );
    // A disabled affordance explains itself regardless of role.
    assert_eq!(
        fork_tooltip(
            MessageRole::User,
            &ForkGate::Disabled("Only Pi chats can be forked.")
        ),
        "Only Pi chats can be forked."
    );
}

// ---- Session Rewind (restart from a message) ----

#[test]
fn rewind_gate_matches_the_fork_prerequisites_plus_the_tail_rule() {
    // A settled Pi root chat, anchored anywhere but the newest entry.
    assert_eq!(
        rewind_gate(
            false,
            Some(&pi_chat(false, false)),
            false,
            false,
            true,
            false
        ),
        ForkGate::Enabled
    );
    // The newest entry has nothing after it: restarting would delete
    // nothing, so the affordance is inert and says why.
    assert_eq!(
        rewind_gate(
            false,
            Some(&pi_chat(false, false)),
            false,
            false,
            true,
            true
        ),
        ForkGate::Disabled("Nothing to remove after the last message.")
    );
    // Every fork prerequisite still applies, worded for a restart.
    assert_eq!(
        rewind_gate(
            true,
            Some(&pi_chat(false, false)),
            false,
            false,
            true,
            false
        ),
        ForkGate::Disabled("Side chats can't be restarted from a message.")
    );
    assert_eq!(
        rewind_gate(
            false,
            Some(&pi_chat(false, true)),
            false,
            false,
            true,
            false
        ),
        ForkGate::Disabled("Only Pi chats can be restarted from a message.")
    );
    assert_eq!(
        rewind_gate(
            false,
            Some(&pi_chat(true, false)),
            false,
            false,
            true,
            false
        ),
        ForkGate::Disabled("Subagent chats can't be restarted from a message.")
    );
    assert_eq!(
        rewind_gate(
            false,
            Some(&pi_chat(false, false)),
            true,
            false,
            true,
            false
        ),
        ForkGate::Disabled("Wait for the chat to finish before restarting it.")
    );
    assert_eq!(
        rewind_gate(
            false,
            Some(&pi_chat(false, false)),
            false,
            true,
            true,
            false
        ),
        ForkGate::Disabled("The engine is offline.")
    );
    assert_eq!(
        rewind_gate(
            false,
            Some(&pi_chat(false, false)),
            false,
            false,
            false,
            false
        ),
        ForkGate::Disabled("The device hosting this chat is offline.")
    );
    assert_eq!(
        rewind_gate(false, None, false, false, true, false),
        ForkGate::Disabled("No chat selected.")
    );
}

#[test]
fn rewind_tooltip_names_the_damage_once_armed() {
    // Unarmed: what the affordance does, by role.
    assert_eq!(
        rewind_tooltip(MessageRole::User, &ForkGate::Enabled, false, 3),
        "Restart from here — deletes this message and everything after it"
    );
    assert_eq!(
        rewind_tooltip(MessageRole::Assistant, &ForkGate::Enabled, false, 3),
        "Restart from here — deletes everything after this response"
    );
    // Armed: the confirming click's exact cost, correctly pluralized.
    assert_eq!(
        rewind_tooltip(MessageRole::User, &ForkGate::Enabled, true, 3),
        "Click again to delete this message and 3 messages after it"
    );
    assert_eq!(
        rewind_tooltip(MessageRole::Assistant, &ForkGate::Enabled, true, 1),
        "Click again to delete 1 message after this response"
    );
    // A disabled affordance explains itself, armed or not.
    assert_eq!(
        rewind_tooltip(
            MessageRole::User,
            &ForkGate::Disabled("The engine is offline."),
            true,
            3
        ),
        "The engine is offline."
    );
}

#[test]
fn rows_carry_their_entry_role_for_fork_gating() {
    // Every row inherits its entry's role — System rows (which must never
    // emit a fork affordance) are distinguishable from Assistant rows.
    for (role, id) in [
        (MessageRole::User, "u1"),
        (MessageRole::Assistant, "a1"),
        (MessageRole::System, "s1"),
    ] {
        let entry = SessionMessageEntry {
            id: id.into(),
            role,
            parts: vec![text_part("t0", "content")],
            status: Some(MessageStatus::Complete),
            ..crate::test_fixtures::entry()
        };
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert!(!rows.is_empty(), "{id} renders a row");
        assert!(
            rows.iter().all(|r| r.role == role),
            "{id}: every row carries the entry role"
        );
    }
    // A tool-only assistant entry still carries the assistant role on its
    // ToolGroup row (System rows gate the affordance by role, not kind).
    let tool_entry = SessionMessageEntry {
        id: "a2".into(),
        role: MessageRole::Assistant,
        parts: vec![tool_part("t1", "cargo test")],
        status: Some(MessageStatus::Complete),
        ..crate::test_fixtures::entry()
    };
    let rows = rows_for_entry(&tool_entry, false, &mut parse);
    assert!(matches!(rows[0].kind, RowKind::ToolGroup { .. }));
    assert_eq!(rows[0].role, MessageRole::Assistant);
}
