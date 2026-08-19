//! Text selection for rendered markdown (round 18).
//!
//! gpui has no built-in selection for plain text elements. Zed's markdown
//! selects continuously because its whole document is ONE element over one
//! text model; zeron renders a TREE of text elements inside a virtualized
//! list, so this module rebuilds that continuity: every frame the renderer
//! registers each painted text element in paint order (= document order),
//! and a drag anchored in one element resolves against that registry into
//! per-element SPANS — partial in the anchor/head elements, whole for every
//! element between. The wash paints per element from its span; copy joins
//! the spans in order.
//!
//! This module is the pure state half (gpui-free, unit-tested); the
//! registry, geometry and mouse listeners live in `render.rs`.

use std::ops::Range;
use std::sync::{Mutex, OnceLock};

/// One element's slice of the selection, in document order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Element key (`{row_key}:{element ix}`).
    pub key: String,
    /// Selected byte range of the element's flat text.
    pub range: Range<usize>,
    /// The element's full flat text (copy source, snapshotted at drag time
    /// so copy still works after the element scrolls out of the registry).
    pub text: String,
}

/// A settled selection snapshot, captured at mouse-up: the ordered spans
/// (copy source), the joined visible text, and the drag's HEAD — the element
/// key + byte offset where the mouse last was. The head anchors the
/// transcript's Comment pill at the selection endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSnapshot {
    /// Element key (`{row_key}:{element ix}`) under the head.
    pub head_key: String,
    /// Byte offset of the head within its element.
    pub head_ix: usize,
    /// Resolved spans, document order (empty only for a degenerate selection).
    pub spans: Vec<Span>,
    /// Spans joined in document order — the exact visible selected quote.
    pub text: String,
}

impl SelectionSnapshot {
    /// The row id the head element belongs to (`{row_key}:{element ix}`) —
    /// the anchor for dismissing the offer when that row is replaced.
    pub fn head_row(&self) -> &str {
        // Assistant Markdown's production keys append `-t{element_ix}`;
        // user bubbles append `:u`. Strip only recognized renderer suffixes
        // so punctuation inside a real row id remains untouched.
        if let Some((row, suffix)) = self.head_key.rsplit_once("-t")
            && !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_digit())
        {
            return row;
        }
        if let Some((row, suffix)) = self.head_key.rsplit_once(':')
            && (suffix == "u" || (!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())))
        {
            return row;
        }
        &self.head_key
    }
}

#[derive(Clone, Default)]
struct MdSelection {
    /// Element that owns the drag (where the mouse went down).
    anchor_key: String,
    /// Byte offset of the anchor within its element.
    anchor_ix: usize,
    dragging: bool,
    /// Double/triple-click selections are already complete spans and must not
    /// be replaced by incidental pointer movement before mouse-up.
    fixed_span: bool,
    /// Resolved spans, document order. Empty while a click hasn't moved.
    spans: Vec<Span>,
    /// Element key under the drag's head (the mouse's last position).
    head_key: String,
    /// Byte offset of the head within its element.
    head_ix: usize,
}

fn state() -> &'static Mutex<Option<MdSelection>> {
    static STATE: OnceLock<Mutex<Option<MdSelection>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// Resolve the spans for a selection between `a` and `b`, each an
/// `(element index, byte offset)` into `elements` (document-ordered
/// `(key, text)` pairs). Handles either direction; empty slices are skipped.
pub fn resolve_spans(elements: &[(&str, &str)], a: (usize, usize), b: (usize, usize)) -> Vec<Span> {
    let (start, end) = if (a.0, a.1) <= (b.0, b.1) {
        (a, b)
    } else {
        (b, a)
    };
    let mut spans = Vec::new();
    for (ei, (key, text)) in elements.iter().enumerate().take(end.0 + 1).skip(start.0) {
        let from = if ei == start.0 { start.1 } else { 0 };
        let to = if ei == end.0 { end.1 } else { text.len() };
        let (from, to) = (from.min(text.len()), to.min(text.len()));
        if from < to {
            spans.push(Span {
                key: (*key).to_string(),
                range: from..to,
                text: (*text).to_string(),
            });
        }
    }
    spans
}

/// Begin a drag anchored at `(key, ix)`; claims the global selection.
pub fn begin(key: &str, ix: usize) {
    *state().lock().unwrap() = Some(MdSelection {
        anchor_key: key.to_string(),
        anchor_ix: ix,
        dragging: true,
        fixed_span: false,
        spans: Vec::new(),
        head_key: key.to_string(),
        head_ix: ix,
    });
}

/// Begin with an immediate span (double/triple click inside one element).
pub fn begin_with_span(key: &str, text: &str, range: Range<usize>) {
    let head_ix = range.end;
    *state().lock().unwrap() = Some(MdSelection {
        anchor_key: key.to_string(),
        anchor_ix: range.start,
        dragging: true,
        fixed_span: true,
        spans: vec![Span {
            key: key.to_string(),
            range,
            text: text.to_string(),
        }],
        // The head lands at the span's right edge — the natural "endpoint"
        // of a word/paragraph selection with no drag to track.
        head_key: key.to_string(),
        head_ix,
    });
}

/// The live drag's anchor, if `key` owns it: `(anchor byte offset)`.
pub fn drag_anchor(key: &str) -> Option<usize> {
    let guard = state().lock().unwrap();
    let sel = guard.as_ref()?;
    (sel.dragging && sel.anchor_key == key).then_some(sel.anchor_ix)
}

/// Whether `key` owns a fixed double/triple-click span. Fixed spans settle as
/// selected, without a final character-level drag update.
pub fn drag_is_fixed(key: &str) -> bool {
    state()
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|sel| sel.dragging && sel.anchor_key == key && sel.fixed_span)
}

/// Replace the resolved spans + drag head (drag update). Returns true if
/// anything changed (repaint gate). `head_key` is the element under the
/// mouse; it always trails the span resolution in the same frame. A FIXED
/// double/triple-click span is never replaced here — the renderer skips
/// drag updates for it ([`Self::drag_is_fixed`]), and this guard makes the
/// invariant hold even if a stray caller resolves against its position.
pub fn update_drag(head_key: &str, head_ix: usize, spans: Vec<Span>) -> bool {
    let mut guard = state().lock().unwrap();
    let Some(sel) = guard.as_mut() else {
        return false;
    };
    if sel.fixed_span {
        return false;
    }
    if sel.spans == spans && sel.head_key == head_key && sel.head_ix == head_ix {
        return false;
    }
    sel.head_key = head_key.to_string();
    sel.head_ix = head_ix;
    sel.spans = spans;
    true
}

/// End the drag for `key`'s claim; returns the settled snapshot if the
/// selection is non-empty. The state stays (settled) so copy + the wash
/// keep working; [`SelectionSnapshot::text`] is the joined visible quote.
pub fn end_drag(key: &str) -> Option<SelectionSnapshot> {
    let mut guard = state().lock().unwrap();
    let sel = guard.as_mut()?;
    if sel.anchor_key != key || !sel.dragging {
        return None;
    }
    sel.dragging = false;
    if sel.spans.iter().all(|s| s.range.is_empty()) {
        *guard = None;
        return None;
    }
    Some(SelectionSnapshot {
        head_key: sel.head_key.clone(),
        head_ix: sel.head_ix,
        spans: sel.spans.clone(),
        text: join_spans(&sel.spans),
    })
}

/// Unconditionally drop the selection (chat switch, row replacement).
pub fn clear() {
    *state().lock().unwrap() = None;
}

/// Clear if `key` owns a settled selection (a mouse-down landed outside the
/// owner; the element the down landed IN claims right after). True if cleared.
pub fn clear_if_owner(key: &str) -> bool {
    let mut guard = state().lock().unwrap();
    if guard
        .as_ref()
        .is_some_and(|s| s.anchor_key == key && !s.dragging)
    {
        *guard = None;
        return true;
    }
    false
}

/// The wash range for `key` this frame (empty ⇒ nothing to paint).
pub fn wash_range(key: &str) -> Option<Range<usize>> {
    let guard = state().lock().unwrap();
    let sel = guard.as_ref()?;
    sel.spans
        .iter()
        .find(|s| s.key == key && !s.range.is_empty())
        .map(|s| s.range.clone())
}

/// The full selected text (Cmd+C), spans joined in document order.
pub fn selected_text() -> Option<String> {
    let guard = state().lock().unwrap();
    let sel = guard.as_ref()?;
    if sel.spans.iter().all(|s| s.range.is_empty()) {
        return None;
    }
    Some(join_spans(&sel.spans))
}

fn join_spans(spans: &[Span]) -> String {
    spans
        .iter()
        .filter(|s| !s.range.is_empty())
        .map(|s| &s.text[s.range.clone()])
        .collect::<Vec<_>>()
        .join("\n")
}

/// Word range around `ix` for double-click selection: an alphanumeric/`_`
/// run, or the single non-space char under the cursor, or empty at spaces.
pub fn word_range(text: &str, ix: usize) -> Range<usize> {
    let mut ix = ix.min(text.len());
    // Snap into a char boundary (mouse indices should already be on one;
    // defensive against mid-char byte offsets).
    while ix > 0 && !text.is_char_boundary(ix) {
        ix -= 1;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let before = text[..ix].chars().next_back();
    let at = text[ix..].chars().next();
    // Off a word boundary entirely: select the single char (or nothing).
    if !at.is_some_and(is_word) && !before.is_some_and(is_word) {
        return match at {
            Some(c) if !c.is_whitespace() => ix..ix + c.len_utf8(),
            _ => ix..ix,
        };
    }
    let start = text[..ix]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(ix);
    let end = text[ix..]
        .char_indices()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, c)| ix + i + c.len_utf8())
        .unwrap_or(ix);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn state_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn drag_lifecycle_and_copy_joins() {
        let _state = state_lock();
        begin("p1", 6);
        assert_eq!(drag_anchor("p1"), Some(6));
        assert_eq!(drag_anchor("p2"), None);
        let spans = resolve_spans(&elems(), (0, 6), (1, 6));
        assert!(update_drag("p2", 6, spans.clone()));
        assert!(!update_drag("p2", 6, spans)); // unchanged ⇒ no repaint
        assert_eq!(wash_range("p1"), Some(6..15));
        assert_eq!(wash_range("p2"), Some(0..6));
        assert_eq!(wash_range("p3"), None);
        let snapshot = end_drag("p1").expect("settled snapshot");
        // Head tracks the drag's last position (element p2, offset 6).
        assert_eq!(snapshot.head_key, "p2");
        assert_eq!(snapshot.head_ix, 6);
        assert_eq!(snapshot.head_row(), "p2");
        assert_eq!(snapshot.text, "paragraph\nsecond");
        assert_eq!(selected_text().as_deref(), Some("paragraph\nsecond"));
        // Settled: a down elsewhere clears via the owner's listener.
        assert!(!clear_if_owner("p2"));
        assert!(clear_if_owner("p1"));
        assert_eq!(selected_text(), None);
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
    }

    #[test]
    fn empty_click_clears_on_release() {
        let _state = state_lock();
        begin("p1", 3);
        assert_eq!(end_drag("p1"), None);
        assert_eq!(selected_text(), None);
    }

    #[test]
    fn double_click_span_heads_the_range_end() {
        let _state = state_lock();
        begin_with_span("p1", "hello world", 6..11);
        assert!(drag_is_fixed("p1"));
        assert_eq!(wash_range("p1"), Some(6..11));
        let snapshot = end_drag("p1").expect("settled");
        assert_eq!(snapshot.text, "world");
        assert_eq!(snapshot.head_ix, 11);
    }

    #[test]
    fn fixed_span_survives_incidental_updates() {
        // A double/triple-click span is complete at mouse-down: an incidental
        // MouseMove or the mouse-up character resolution must not overwrite it.
        let _state = state_lock();
        begin_with_span("p1", "hello world", 6..11);
        // A stray drag update at a different element/offset changes nothing.
        assert!(!update_drag(
            "p2",
            0,
            resolve_spans(&elems(), (0, 0), (0, 5))
        ));
        assert!(drag_is_fixed("p1"));
        assert_eq!(wash_range("p1"), Some(6..11));
        // The head stays at the span's right edge, not at the stray point.
        let snapshot = end_drag("p1").expect("settled");
        assert_eq!(snapshot.head_key, "p1");
        assert_eq!(snapshot.head_ix, 11);
        assert_eq!(snapshot.text, "world");
        // A simple drag (non-fixed) still accepts updates normally.
        begin("p1", 0);
        assert!(update_drag(
            "p2",
            6,
            resolve_spans(&elems(), (0, 0), (1, 6))
        ));
        assert!(!drag_is_fixed("p1"));
    }

    #[test]
    fn unicode_reversed_cross_element_snapshot() {
        // A bottom-up drag across two elements normalizes to document order
        // in the snapshot while the head stays at the drag's final position.
        let _state = state_lock();
        let u = [("é1", "héllo wörld"), ("é2", "café")];
        let spans = resolve_spans(&u, (1, 3), (0, 7)); // reversed, char-safe
        let mut guard = state().lock().unwrap();
        *guard = Some(MdSelection {
            anchor_key: "é1".into(),
            anchor_ix: 7,
            dragging: true,
            fixed_span: false,
            spans: spans.clone(),
            head_key: "é2".into(),
            head_ix: 3,
        });
        drop(guard);
        assert_eq!(wash_range("é1"), Some(7..13));
        let snapshot = end_drag("é1").expect("settled");
        assert_eq!(snapshot.text, "wörld\ncaf");
        assert_eq!(snapshot.head_key, "é2");
        assert_eq!(snapshot.spans[0].text, "héllo wörld");
    }

    #[test]
    fn clear_drops_everything() {
        let _state = state_lock();
        begin_with_span("p1", "hello world", 6..11);
        assert!(selected_text().is_some());
        clear();
        assert_eq!(selected_text(), None);
        assert_eq!(end_drag("p1"), None);
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
}
