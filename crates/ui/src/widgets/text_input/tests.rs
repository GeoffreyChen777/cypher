use super::*;

#[test]
fn ime_selection_is_measured_inside_the_composing_text() {
    // What `setMarkedText:selectedRange:` hands over: the caret sits at the
    // end of "zai jia", the composition starts after "中文", and the draft
    // reads "中文zai jia English sentence".
    let draft = "中文zai jia English sentence";
    let new_text = "zai jia";
    let start = "中文".len();
    let caret = start + utf16_to_byte_offset(new_text, 7);
    assert_eq!(caret, start + new_text.len());
    assert_eq!(&draft[..caret], "中文zai jia");
    // Resolving the same offset against the whole draft is the drift: two
    // CJK characters push it four bytes too far.
    assert_eq!(start + utf16_to_byte_offset(draft, 7), caret + 4);
}

#[test]
fn utf16_offsets_resolve_per_string() {
    assert_eq!(utf16_to_byte_offset("abc", 2), 2);
    assert_eq!(utf16_to_byte_offset("中文abc", 2), 6);
    assert_eq!(utf16_to_byte_offset("中文abc", 3), 7);
    // Surrogate pairs count as two UTF-16 units and four bytes.
    assert_eq!(utf16_to_byte_offset("😀a", 2), 4);
    assert_eq!(utf16_to_byte_offset("😀a", 3), 5);
    // Past the end clamps to the end rather than running off it.
    assert_eq!(utf16_to_byte_offset("中文", 99), 6);
    assert_eq!(utf16_to_byte_offset("", 3), 0);
}

fn tooltip_target(range: Range<usize>, path: &str) -> ChipTooltipTarget {
    ChipTooltipTarget {
        range,
        label: path.into(),
    }
}

#[test]
fn mention_tooltip_wait_survives_pointer_jitter_and_promotes_once() {
    let target = tooltip_target(3..20, "src/composer.rs");
    let waiting = ChipTooltipPhase::Waiting {
        target: target.clone(),
        generation: 1,
    };
    let restarted = chip_tooltip_reduce(waiting.clone(), Some(target.clone()), false, 2);
    assert_eq!(restarted, waiting);
    assert!(matches!(
        restarted,
        ChipTooltipPhase::Waiting { generation: 1, .. }
    ));
    assert_eq!(
        chip_tooltip_promote(restarted.clone(), 2, true),
        restarted,
        "a stale timer must not reveal the tooltip"
    );
    let visible = chip_tooltip_promote(restarted, 1, true);
    assert!(matches!(
        visible,
        ChipTooltipPhase::Visible { generation: 1, .. }
    ));
    assert_eq!(
        chip_tooltip_reduce(visible.clone(), Some(target), false, 3),
        visible,
        "one visible activation keeps its presentation generation stable"
    );
}

#[test]
fn mention_tooltip_changes_target_and_cancels_disappeared_target() {
    let first = tooltip_target(0..10, "src/a.rs");
    let second = tooltip_target(20..30, "src/a.rs");
    let visible = ChipTooltipPhase::Visible {
        target: first,
        generation: 4,
    };
    assert!(matches!(
        chip_tooltip_reduce(visible, Some(second), false, 5),
        ChipTooltipPhase::Waiting { generation: 5, .. }
    ));
    assert_eq!(
        chip_tooltip_promote(
            ChipTooltipPhase::Waiting {
                target: tooltip_target(20..30, "src/a.rs"),
                generation: 5,
            },
            5,
            false,
        ),
        ChipTooltipPhase::Hidden
    );
}

#[test]
fn mention_tooltip_stays_visible_over_chip_or_popup_only() {
    assert!(chip_tooltip_contains(true, false));
    assert!(chip_tooltip_contains(false, true));
    assert!(!chip_tooltip_contains(false, false));
}

#[test]
fn mention_wash_moves_wholly_to_the_next_visual_row_at_a_wrap() {
    assert_eq!(
        display_row_segments(12..24, [12, 40]),
        vec![(1, 12, 12..24)]
    );
    assert_eq!(
        display_row_segments(8..24, [12, 40]),
        vec![(0, 0, 8..12), (1, 12, 12..24)]
    );
}

#[test]
fn secret_projection_masks_unicode_and_preserves_caret_boundaries() {
    let raw = "sk-密🔑e";
    let projection = TextProjection::secret(raw);
    assert_eq!(projection.display, "••••••");
    assert!(projection.chips.is_empty());
    for (index, (offset, _)) in raw.char_indices().enumerate() {
        assert_eq!(projection.raw_to_display(offset), index * '•'.len_utf8());
        assert_eq!(projection.display_to_raw(index * '•'.len_utf8()), offset);
    }
    assert_eq!(
        projection.raw_to_display(raw.len()),
        projection.display.len()
    );
    assert_eq!(
        projection.display_to_raw(projection.display.len()),
        raw.len()
    );
    let empty = TextProjection::secret("");
    assert_eq!(empty.raw_to_display(0), 0);
    assert_eq!(empty.display_to_raw(0), 0);
}

#[test]
fn caret_blink_phase() {
    // Solid through the first half-period (typing burst never blinks).
    assert!(caret_visible(0));
    assert!(caret_visible(CARET_BLINK_MS - 1));
    // Off for the second half-period, back on for the third.
    assert!(!caret_visible(CARET_BLINK_MS));
    assert!(!caret_visible(2 * CARET_BLINK_MS - 1));
    assert!(caret_visible(2 * CARET_BLINK_MS));
}

#[test]
fn input_wheel_scroll_uses_gpui_direction_and_clamps() {
    // Positive wheel delta moves toward the start; negative moves down.
    assert_eq!(input_scroll_offset(40.0, 20.0, 200.0, 100.0), 20.0);
    assert_eq!(input_scroll_offset(40.0, -30.0, 200.0, 100.0), 70.0);
    // Neither edge can be overscrolled.
    assert_eq!(input_scroll_offset(10.0, 50.0, 200.0, 100.0), 0.0);
    assert_eq!(input_scroll_offset(90.0, -50.0, 200.0, 100.0), 100.0);
    // Short content has no internal scroll range.
    assert_eq!(input_scroll_offset(20.0, -50.0, 80.0, 100.0), 0.0);
}

#[test]
fn input_scroll_reveals_only_when_caret_leaves_viewport() {
    // A visible caret preserves the user's viewport.
    assert_eq!(
        input_scroll_offset_for_cursor(40.0, 60.0, 20.0, 300.0, 100.0),
        40.0
    );
    // Moving above or below reveals the row with the smallest adjustment.
    assert_eq!(
        input_scroll_offset_for_cursor(80.0, 30.0, 20.0, 300.0, 100.0),
        30.0
    );
    assert_eq!(
        input_scroll_offset_for_cursor(20.0, 130.0, 20.0, 300.0, 100.0),
        50.0
    );
    // Revealing the final row clamps exactly to the content end.
    assert_eq!(
        input_scroll_offset_for_cursor(0.0, 290.0, 20.0, 300.0, 100.0),
        200.0
    );
}

#[test]
fn input_drag_autoscroll_is_edge_proportional_and_capped() {
    let top = 100.0;
    let bottom = 300.0;
    let line = INPUT_LINE_HEIGHT;
    assert_eq!(input_drag_scroll_delta(200.0, top, bottom, line), 0.0);
    assert_eq!(input_drag_scroll_delta(90.0, top, bottom, line), -2.0);
    assert_eq!(input_drag_scroll_delta(315.0, top, bottom, line), 3.0);
    assert_eq!(input_drag_scroll_delta(-100.0, top, bottom, line), -line);
    assert_eq!(input_drag_scroll_delta(500.0, top, bottom, line), line);
}
