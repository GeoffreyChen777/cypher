use super::*;

fn mods() -> Modifiers {
    Modifiers::default()
}

#[test]
fn printables_prefer_key_char() {
    assert_eq!(
        keystroke_bytes("a", Some("a"), &mods(), false),
        Some(b"a".to_vec())
    );
    assert_eq!(
        keystroke_bytes(
            "a",
            Some("A"),
            &Modifiers {
                shift: true,
                ..mods()
            },
            false
        ),
        Some(b"A".to_vec())
    );
    // Multi-byte characters pass through as UTF-8.
    assert_eq!(
        keystroke_bytes("e", Some("é"), &mods(), false),
        Some("é".as_bytes().to_vec())
    );
    // Named single-char keys fall back to the key name.
    assert_eq!(
        keystroke_bytes("/", None, &mods(), false),
        Some(b"/".to_vec())
    );
    // Unknown multi-char keys are not ours.
    assert_eq!(keystroke_bytes("capslock", None, &mods(), false), None);
}

#[test]
fn control_keys_and_sequences() {
    assert_eq!(
        keystroke_bytes("enter", None, &mods(), false),
        Some(b"\r".to_vec())
    );
    assert_eq!(
        keystroke_bytes("backspace", None, &mods(), false),
        Some(vec![0x7f])
    );
    assert_eq!(
        keystroke_bytes("tab", None, &mods(), false),
        Some(b"\t".to_vec())
    );
    assert_eq!(
        keystroke_bytes(
            "tab",
            None,
            &Modifiers {
                shift: true,
                ..mods()
            },
            false
        ),
        Some(b"\x1b[Z".to_vec())
    );
    assert_eq!(
        keystroke_bytes("escape", None, &mods(), false),
        Some(vec![0x1b])
    );
    assert_eq!(
        keystroke_bytes("delete", None, &mods(), false),
        Some(b"\x1b[3~".to_vec())
    );
    assert_eq!(
        keystroke_bytes("pageup", None, &mods(), false),
        Some(b"\x1b[5~".to_vec())
    );
    assert_eq!(
        keystroke_bytes("f5", None, &mods(), false),
        Some(b"\x1b[15~".to_vec())
    );
}

#[test]
fn arrows_respect_app_cursor_mode() {
    assert_eq!(
        keystroke_bytes("up", None, &mods(), false),
        Some(b"\x1b[A".to_vec())
    );
    assert_eq!(
        keystroke_bytes("up", None, &mods(), true),
        Some(b"\x1bOA".to_vec())
    );
    assert_eq!(
        keystroke_bytes("home", None, &mods(), false),
        Some(b"\x1b[H".to_vec())
    );
    assert_eq!(
        keystroke_bytes("end", None, &mods(), true),
        Some(b"\x1bOF".to_vec())
    );
}

#[test]
fn ctrl_combos_map_to_control_bytes() {
    let ctrl = Modifiers {
        control: true,
        ..mods()
    };
    assert_eq!(
        keystroke_bytes("c", Some("c"), &ctrl, false),
        Some(vec![0x03])
    );
    assert_eq!(keystroke_bytes("z", None, &ctrl, false), Some(vec![0x1a]));
    assert_eq!(
        keystroke_bytes("space", None, &ctrl, false),
        Some(vec![0x00])
    );
    assert_eq!(keystroke_bytes("[", None, &ctrl, false), Some(vec![0x1b]));
    assert_eq!(keystroke_bytes("_", None, &ctrl, false), Some(vec![0x1f]));
    // Ctrl+1 has no caret encoding — not ours.
    assert_eq!(keystroke_bytes("1", Some("1"), &ctrl, false), None);
}

#[test]
fn alt_prefixes_escape() {
    let alt = Modifiers {
        alt: true,
        ..mods()
    };
    assert_eq!(
        keystroke_bytes("b", Some("b"), &alt, false),
        Some(vec![0x1b, b'b'])
    );
    let alt_ctrl = Modifiers {
        alt: true,
        control: true,
        ..mods()
    };
    assert_eq!(
        keystroke_bytes("c", None, &alt_ctrl, false),
        Some(vec![0x1b, 0x03])
    );
}

#[test]
fn platform_primary_combos_fall_through() {
    let cmd = Modifiers {
        platform: true,
        ..mods()
    };
    assert_eq!(keystroke_bytes("j", Some("j"), &cmd, false), None);
    assert_eq!(keystroke_bytes("enter", None, &cmd, false), None);
}

#[test]
fn paste_wraps_when_bracketed() {
    assert_eq!(paste_bytes("hi", false), b"hi".to_vec());
    assert_eq!(paste_bytes("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
    // Close-bracket injection is stripped.
    assert_eq!(
        paste_bytes("a\x1b[201~rm -rf", true),
        b"\x1b[200~arm -rf\x1b[201~".to_vec()
    );
}

#[test]
fn coalescer_schedules_once_per_burst() {
    let mut c = InputCoalescer::default();
    assert!(c.is_empty());
    assert!(c.push(b"a"), "first push schedules the flush");
    assert!(!c.push(b"b"), "subsequent pushes ride the pending flush");
    assert!(!c.push(b"c"));
    assert_eq!(c.take(), b"abc".to_vec());
    assert!(c.is_empty());
    // Next burst schedules again.
    assert!(c.push(b"d"));
    // Empty pushes never schedule.
    let mut c = InputCoalescer::default();
    assert!(!c.push(b""));
}

#[test]
fn timing_constants_match_spec() {
    assert_eq!(COALESCE_MS, 12);
    assert_eq!(RESIZE_DEBOUNCE_MS, 80);
}

// ---- pointer → cell ----

/// 10x20 cells, an 8x4 grid: cols 0..7, rows 0..3.
fn hit(x: f32, y: f32) -> CellHit {
    cell_at(x, y, 10.0, 20.0, 8, 4)
}

#[test]
fn pointer_maps_to_the_cell_it_is_over() {
    assert_eq!(
        hit(0.0, 0.0),
        CellHit {
            row: 0,
            col: 0,
            side: Side::Left
        }
    );
    assert_eq!(
        hit(25.0, 45.0),
        CellHit {
            row: 2,
            col: 2,
            side: Side::Left
        }
    );
    // Last cell, exactly.
    assert_eq!(
        hit(70.0, 60.0),
        CellHit {
            row: 3,
            col: 7,
            side: Side::Left
        }
    );
}

#[test]
fn side_splits_the_cell_at_its_midpoint() {
    // Cell 2 spans x 20..30, so the midpoint is 25.
    assert_eq!(hit(21.0, 0.0).side, Side::Left);
    assert_eq!(
        hit(25.0, 0.0).side,
        Side::Left,
        "the midpoint itself is left"
    );
    assert_eq!(hit(26.0, 0.0).side, Side::Right);
    // The cell is unaffected by which half.
    assert_eq!(hit(21.0, 0.0).col, 2);
    assert_eq!(hit(29.0, 0.0).col, 2);
}

/// Dragging out of the panel must extend to the edge it left through, not
/// freeze at the last sample inside.
#[test]
fn overshoot_clamps_into_the_grid() {
    assert_eq!(hit(9_999.0, 0.0).col, 7);
    assert_eq!(hit(0.0, 9_999.0).row, 3);
    assert_eq!(hit(-50.0, 0.0).col, 0);
    assert_eq!(hit(0.0, -50.0).row, 0);
}

/// The part clamping alone does not give you: past the right or bottom
/// edge the side is forced Right so the last cell is *included*, and above
/// the top it is forced Left. Dragging below-and-left must still take the
/// bottom row whole.
#[test]
fn overshoot_forces_the_side_to_the_edge() {
    assert_eq!(hit(9_999.0, 10.0).side, Side::Right);
    // x sits in cell 0's left half, but the row overshot — Right wins.
    assert_eq!(hit(1.0, 9_999.0).side, Side::Right);
    assert_eq!(hit(1.0, 9_999.0).col, 0);
    // Above the top, mirrored.
    assert_eq!(hit(75.0, -50.0).side, Side::Left);
}

#[test]
fn degenerate_metrics_do_not_panic() {
    assert_eq!(
        cell_at(5.0, 5.0, 0.0, 20.0, 8, 4),
        CellHit {
            row: 0,
            col: 0,
            side: Side::Left
        }
    );
    assert_eq!(
        cell_at(5.0, 5.0, 10.0, 20.0, 0, 0),
        CellHit {
            row: 0,
            col: 0,
            side: Side::Left
        }
    );
    assert_eq!(cell_at(f32::NAN, f32::INFINITY, 10.0, 20.0, 8, 4).col, 0);
}

#[test]
fn drag_threshold_matches_the_gpui_default() {
    assert_eq!(SELECTION_DRAG_THRESHOLD, 2.0);
}
