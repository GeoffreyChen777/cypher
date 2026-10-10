use super::*;

#[test]
fn diff_quotes_keep_real_empty_source_lines() {
    let rows = [
        ("old-0", "before"),
        ("old-1", ""),
        ("old-2", ""),
        ("old-3", "after"),
    ];
    for (start, end) in [((0, 0), (3, 5)), ((3, 5), (0, 0))] {
        let spans = resolve_line_spans(&rows, start, end);
        assert_eq!(join_spans(&spans), "before\n\n\nafter");
        assert_eq!(spans.len(), 4);
    }
    // Paragraph selection keeps its original empty-element behavior.
    assert_eq!(resolve_spans(&rows, (0, 0), (3, 5)).len(), 2);
}

const S: SelectionScope = SelectionScope::Transcript(999);
// A fixed pane id — real panes allocate via `next_change_scope`, but the
// pure state tests just need two distinct scopes.
const C: SelectionScope = SelectionScope::Changes(999);

fn elems<'a>() -> Vec<(&'a str, &'a str)> {
    vec![
        ("p1", "first paragraph"),
        ("p2", "second"),
        ("p3", "third one"),
    ]
}

#[test]
fn spans_within_one_element() {
    let spans = resolve_spans(&elems(), (0, 6), (0, 15));
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].key, "p1");
    assert_eq!(&spans[0].text[spans[0].range.clone()], "paragraph");
    // Reversed direction normalizes.
    assert_eq!(resolve_spans(&elems(), (0, 15), (0, 6)), spans);
}

#[test]
fn spans_across_elements_cover_middles_whole() {
    let spans = resolve_spans(&elems(), (0, 6), (2, 5));
    assert_eq!(spans.len(), 3);
    assert_eq!(&spans[0].text[spans[0].range.clone()], "paragraph");
    assert_eq!(&spans[1].text[spans[1].range.clone()], "second");
    assert_eq!(&spans[2].text[spans[2].range.clone()], "third");
    // Reversed drag (bottom-up) resolves identically.
    assert_eq!(resolve_spans(&elems(), (2, 5), (0, 6)), spans);
}

/// The drag tests below mutate the process-global selection state —
/// serialize them, or the parallel test runner interleaves their
/// begin/end_drag calls (long-standing flake).
pub fn state_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn drag_lifecycle_and_copy_joins() {
    let _state = state_lock();
    begin(S, "p1", 6);
    assert_eq!(drag_anchor(S, "p1"), Some(6));
    assert_eq!(drag_anchor(S, "p2"), None);
    let spans = resolve_spans(&elems(), (0, 6), (1, 6));
    assert!(update_drag(S, "p2", 6, spans.clone()));
    assert!(!update_drag(S, "p2", 6, spans)); // unchanged ⇒ no repaint
    assert_eq!(wash_range(S, "p1"), Some(6..15));
    assert_eq!(wash_range(S, "p2"), Some(0..6));
    assert_eq!(wash_range(S, "p3"), None);
    let snapshot = end_drag(S, "p1").expect("settled snapshot");
    // Head tracks the drag's last position (element p2, offset 6).
    assert_eq!(snapshot.head_key, "p2");
    assert_eq!(snapshot.head_ix, 6);
    assert_eq!(snapshot.head_row(), "p2");
    assert_eq!(snapshot.text, "paragraph\nsecond");
    assert_eq!(selected_text().as_deref(), Some("paragraph\nsecond"));
    // Settled: a down elsewhere clears via the owner's listener.
    assert!(!clear_if_owner(S, "p2"));
    assert!(clear_if_owner(S, "p1"));
    assert_eq!(selected_text(), None);
}

#[test]
fn begin_in_any_scope_clears_others_and_becomes_latest() {
    // Copy is single-active: beginning a drag in another scope clears the
    // earlier one entirely, and `selected_text()` returns the latest.
    let _state = state_lock();
    begin(S, "p1", 6);
    assert!(update_drag(
        S,
        "p2",
        6,
        resolve_spans(&elems(), (0, 6), (1, 6))
    ));
    assert_eq!(selected_text().as_deref(), Some("paragraph\nsecond"));
    // A new drag in the diff scope clears the transcript selection.
    begin(C, "d1", 0);
    assert_eq!(drag_anchor(S, "p1"), None, "transcript selection cleared");
    assert_eq!(drag_anchor(C, "d1"), Some(0));
    let spans = resolve_spans(&[("d1", "line one"), ("d2", "line two")], (0, 0), (1, 4));
    assert!(update_drag(C, "d2", 4, spans.clone()));
    assert_eq!(wash_range(S, "p1"), None, "no transcript wash");
    assert_eq!(wash_range(C, "d2"), Some(0..4));
    let snapshot = end_drag(C, "d1").expect("diff snapshot");
    assert_eq!(snapshot.text, "line one\nline");
    assert_eq!(
        selected_text().as_deref(),
        Some("line one\nline"),
        "latest scope wins"
    );
    clear(C);
    assert_eq!(selected_text(), None);
}

#[test]
fn per_pane_change_scopes_are_isolated() {
    // Two distinct diff panes (different allocated ids) behave like the
    // transcript vs a pane: the newest begin owns copy, the older pane's
    // selection is gone, and per-scope geometry never leaks.
    let _state = state_lock();
    let c1 = SelectionScope::Changes(1);
    let c2 = SelectionScope::Changes(2);
    begin(c1, "a1", 0);
    assert_eq!(drag_anchor(c1, "a1"), Some(0));
    begin(c2, "b1", 0);
    assert_eq!(drag_anchor(c1, "a1"), None, "older pane cleared");
    assert_eq!(drag_anchor(c2, "b1"), Some(0));
    let spans = resolve_spans(&[("b1", "x"), ("b2", "y")], (0, 0), (1, 1));
    assert!(update_drag(c2, "b2", 1, spans));
    assert_eq!(wash_range(c2, "b2"), Some(0..1));
    assert_eq!(wash_range(c1, "a1"), None);
    clear(c2);
    assert_eq!(selected_text(), None);
}

#[test]
fn next_change_scope_allocates_unique_ids() {
    let a = next_change_scope();
    let b = next_change_scope();
    assert_ne!(a, b);
    assert!(matches!(a, SelectionScope::Changes(_)));
}

#[test]
fn next_side_chat_scope_allocates_unique_ids() {
    // Each temporary Side Chat transcript gets a fresh
    // scope so it never collides with the main transcript or another
    // simultaneously-visible panel.
    let a = next_side_chat_scope();
    let b = next_side_chat_scope();
    assert_ne!(a, b);
    assert!(matches!(a, SelectionScope::SideChat(_)));
    assert_ne!(a, SelectionScope::Transcript(0));
    assert_ne!(a, SelectionScope::Changes(0));
}

#[test]
fn snapshot_head_row_splits_element_suffix() {
    let assistant = SelectionSnapshot {
        head_key: "m1#t0.0-t3".into(),
        head_ix: 12,
        spans: Vec::new(),
        text: String::new(),
    };
    assert_eq!(assistant.head_row(), "m1#t0.0");
    let user = SelectionSnapshot {
        head_key: "m2:u".into(),
        head_ix: 12,
        spans: Vec::new(),
        text: String::new(),
    };
    assert_eq!(user.head_row(), "m2");
    let legacy = SelectionSnapshot {
        head_key: "e1#p1:3".into(),
        head_ix: 12,
        spans: Vec::new(),
        text: String::new(),
    };
    assert_eq!(legacy.head_row(), "e1#p1");
    let bare = SelectionSnapshot {
        head_key: "row:with-punctuation".into(),
        head_ix: 0,
        spans: Vec::new(),
        text: String::new(),
    };
    assert_eq!(bare.head_row(), "row:with-punctuation");
    // Diff keys (`{owner}:f{file}:h{hunk}:l{line}`) stay whole — the
    // suffix is not a recognized renderer marker.
    let diff = SelectionSnapshot {
        head_key: "changes-1:f0:h2:l7".into(),
        head_ix: 3,
        spans: Vec::new(),
        text: String::new(),
    };
    assert_eq!(diff.head_row(), "changes-1:f0:h2:l7");
}

#[test]
fn empty_click_clears_on_release() {
    let _state = state_lock();
    begin(S, "p1", 3);
    assert_eq!(end_drag(S, "p1"), None);
    assert_eq!(selected_text(), None);
}

#[test]
fn double_click_span_heads_the_range_end() {
    let _state = state_lock();
    begin_with_span(S, "p1", "hello world", 6..11);
    assert!(drag_is_fixed(S, "p1"));
    assert_eq!(wash_range(S, "p1"), Some(6..11));
    let snapshot = end_drag(S, "p1").expect("settled");
    assert_eq!(snapshot.text, "world");
    assert_eq!(snapshot.head_ix, 11);
}

#[test]
fn fixed_span_survives_incidental_updates() {
    // A double/triple-click span is complete at mouse-down: an incidental
    // MouseMove or the mouse-up character resolution must not overwrite it.
    let _state = state_lock();
    begin_with_span(S, "p1", "hello world", 6..11);
    // A stray drag update at a different element/offset changes nothing.
    assert!(!update_drag(
        S,
        "p2",
        0,
        resolve_spans(&elems(), (0, 0), (0, 5))
    ));
    assert!(drag_is_fixed(S, "p1"));
    assert_eq!(wash_range(S, "p1"), Some(6..11));
    // The head stays at the span's right edge, not at the stray point.
    let snapshot = end_drag(S, "p1").expect("settled");
    assert_eq!(snapshot.head_key, "p1");
    assert_eq!(snapshot.head_ix, 11);
    assert_eq!(snapshot.text, "world");
    // A simple drag (non-fixed) still accepts updates normally.
    begin(S, "p1", 0);
    assert!(update_drag(
        S,
        "p2",
        6,
        resolve_spans(&elems(), (0, 0), (1, 6))
    ));
    assert!(!drag_is_fixed(S, "p1"));
}

#[test]
fn unicode_reversed_cross_element_snapshot() {
    // A bottom-up drag across two elements normalizes to document order
    // in the snapshot while the head stays at the drag's final position.
    let _state = state_lock();
    let u = [("é1", "héllo wörld"), ("é2", "café")];
    let spans = resolve_spans(&u, (1, 3), (0, 7)); // reversed, char-safe
    let mut guard = state().lock().unwrap();
    *guard.entry(S).or_default() = Some(MdSelection {
        anchor_key: "é1".into(),
        anchor_ix: 7,
        dragging: true,
        fixed_span: false,
        spans: spans.clone(),
        head_key: "é2".into(),
        head_ix: 3,
    });
    drop(guard);
    assert_eq!(wash_range(S, "é1"), Some(7..13));
    let snapshot = end_drag(S, "é1").expect("settled");
    assert_eq!(snapshot.text, "wörld\ncaf");
    assert_eq!(snapshot.head_key, "é2");
    assert_eq!(snapshot.spans[0].text, "héllo wörld");
}

#[test]
fn clear_drops_everything() {
    let _state = state_lock();
    begin_with_span(S, "p1", "hello world", 6..11);
    assert!(selected_text().is_some());
    clear(S);
    assert_eq!(selected_text(), None);
    assert_eq!(end_drag(S, "p1"), None);
}

#[test]
fn word_ranges() {
    let t = "let foo_bar = 12;";
    assert_eq!(word_range(t, 5), 4..11); // inside foo_bar
    assert_eq!(word_range(t, 4), 4..11); // at word start
    assert_eq!(word_range(t, 11), 4..11); // at word end
    assert_eq!(word_range(t, 15), 14..16); // inside 12
    assert_eq!(&t[word_range(t, 12)], "="); // lone symbol
    assert_eq!(word_range(t, 3), 0..3); // boundary after "let"
    // Unicode-safe (mid-char byte offsets snap down).
    let u = "héllo wörld";
    assert_eq!(&u[word_range(u, 2)], "héllo");
}
