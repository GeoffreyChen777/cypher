//! Text selection for rendered markdown.
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
//! State is SCOPED per surface ([`SelectionScope`]): the transcript and each
//! diff pane select independently, so a drag in one can never be claimed,
//! resolved, or cleared by the other — paint order cannot conflict.
//!
//! This module is the pure state half (gpui-free, unit-tested); the
//! registry, geometry and mouse listeners live in `render.rs`.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Which surface owns a selection. Every stateful call takes the scope so
/// independent surfaces (the transcript, each diff pane) never collide in
/// the shared registry/wash/popup — each surface clears its registry and its
/// selection separately, in its own paint order.
///
/// The SELECTION STATE is single-active: beginning a drag in any scope
/// clears every other scope (see [`begin`]), so copy (`[`selected_text`]`)
/// always returns exactly one quote — the latest gesture — while the paint
/// geometry stays per-scope (a hidden pane can never hijack the active
/// pane's listeners).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SelectionScope {
    /// A conversation transcript (markdown rows + user bubbles), one fresh
    /// id per [`Transcript`](crate::transcript::Transcript) instance
    /// ([`next_transcript_scope`]): several session tiles show transcripts
    /// side by side, and a shared scope made them fight over the selection
    /// and the Comment pill.
    Transcript(u64),
    /// A Git diff surface, allocated fresh ids for each pane and each of its
    /// split columns ([`next_change_scope`]). Tabs/versions never share a
    /// selection registry and closed panes' scopes are never reused.
    Changes(u64),
    /// A temporary Side Chat transcript ([`next_side_chat_scope`]): one fresh
    /// scope per panel, so a side chat beside the main transcript — or two
    /// side chats side by side — never collide in the shared selection
    /// registry. Side-chat scopes render selection + copy but deliberately
    /// offer NO annotation actions (no Comment pill, no nested Side Chat).
    SideChat(u64),
    /// The composer's agent-question card ([`next_question_scope`]): the
    /// question, its context and option copy select + copy like transcript
    /// text, with no annotation actions.
    Question(u64),
    /// A standalone rendered-markdown preview (the Files panel's Markdown
    /// preview, the Appearance chat preview): one fresh scope per surface
    /// ([`next_preview_scope`]), selection + copy only.
    Preview(u64),
}

/// Allocate a fresh per-transcript selection scope (one per session tile's
/// transcript; a dropped transcript's scope is never reused).
pub fn next_transcript_scope() -> SelectionScope {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SelectionScope::Transcript(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Allocate a fresh per-panel Side Chat selection scope. Each temporary
/// Side Chat transcript allocates its own so a panel can never collide with
/// the main transcript or another simultaneously-visible panel, and a closed
/// panel's scope is never reused.
pub fn next_side_chat_scope() -> SelectionScope {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SelectionScope::SideChat(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Allocate a fresh selection scope for one composer's question card.
pub fn next_question_scope() -> SelectionScope {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SelectionScope::Question(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Allocate a fresh selection scope for a standalone markdown preview. The
/// surface must paint [`selection_frame_reset`](super::render::selection_frame_reset)
/// for it before its text, like every other scope.
pub fn next_preview_scope() -> SelectionScope {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SelectionScope::Preview(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Allocate a fresh per-pane diff selection scope. Each [`Changes`](crate::changes::Changes)
/// pane allocates its own so hidden/background panes never collide with the
/// active one, and a closed pane's scope can't be claimed by a new pane.
pub fn next_change_scope() -> SelectionScope {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SelectionScope::Changes(NEXT.fetch_add(1, Ordering::Relaxed))
}

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

/// The row id an element key belongs to (`{row_key}:{element ix}`) — the
/// anchor for dismissing a comment offer when that row is replaced.
/// Assistant Markdown's production keys append `-t{element_ix}`; user
/// bubbles append `:u`. Strip only recognized renderer suffixes so
/// punctuation inside a real row id remains untouched.
pub fn row_of_key(key: &str) -> &str {
    if let Some((row, suffix)) = key.rsplit_once("-t")
        && !suffix.is_empty()
        && suffix.bytes().all(|b| b.is_ascii_digit())
    {
        return row;
    }
    if let Some((row, suffix)) = key.rsplit_once(':')
        && (suffix == "u" || (!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())))
    {
        return row;
    }
    key
}

impl SelectionSnapshot {
    /// The row id the head element belongs to ([`row_of_key`]).
    pub fn head_row(&self) -> &str {
        row_of_key(&self.head_key)
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

fn state() -> &'static Mutex<HashMap<SelectionScope, Option<MdSelection>>> {
    static STATE: OnceLock<Mutex<HashMap<SelectionScope, Option<MdSelection>>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the spans for a selection between `a` and `b`, each an
/// `(element index, byte offset)` into `elements` (document-ordered
/// `(key, text)` pairs). Handles either direction; empty slices are skipped.
pub fn resolve_spans(elements: &[(&str, &str)], a: (usize, usize), b: (usize, usize)) -> Vec<Span> {
    resolve_spans_inner(elements, a, b, false)
}

/// Diff registries contain source lines rather than paragraphs. Keep genuine
/// empty lines between the endpoints; split-view padding is never registered.
pub fn resolve_line_spans(
    elements: &[(&str, &str)],
    a: (usize, usize),
    b: (usize, usize),
) -> Vec<Span> {
    resolve_spans_inner(elements, a, b, true)
}

fn resolve_spans_inner(
    elements: &[(&str, &str)],
    a: (usize, usize),
    b: (usize, usize),
    lines: bool,
) -> Vec<Span> {
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
        if from < to || (lines && text.is_empty() && ei > start.0 && ei < end.0) {
            spans.push(Span {
                key: (*key).to_string(),
                range: from..to,
                text: (*text).to_string(),
            });
        }
    }
    spans
}

/// Begin a drag anchored at `(key, ix)`; claims the selection of `scope`.
/// Because copy is single-active, beginning a drag in ANY scope clears every
/// other scope's selection — only the newest gesture stays selected.
pub fn begin(scope: SelectionScope, key: &str, ix: usize) {
    let mut guard = state().lock().unwrap();
    guard.retain(|s, _| *s == scope);
    guard.insert(
        scope,
        Some(MdSelection {
            anchor_key: key.to_string(),
            anchor_ix: ix,
            dragging: true,
            fixed_span: false,
            spans: Vec::new(),
            head_key: key.to_string(),
            head_ix: ix,
        }),
    );
}

/// Begin with an immediate span (double/triple click inside one element);
/// same single-active semantics as [`begin`].
pub fn begin_with_span(scope: SelectionScope, key: &str, text: &str, range: Range<usize>) {
    let head_ix = range.end;
    let mut guard = state().lock().unwrap();
    guard.retain(|s, _| *s == scope);
    guard.insert(
        scope,
        Some(MdSelection {
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
        }),
    );
}

/// The live drag's anchor, if `key` owns it: `(anchor byte offset)`.
pub fn drag_anchor(scope: SelectionScope, key: &str) -> Option<usize> {
    let guard = state().lock().unwrap();
    let sel = guard.get(&scope)?.as_ref()?;
    (sel.dragging && sel.anchor_key == key).then_some(sel.anchor_ix)
}

/// Whether `key` owns a fixed double/triple-click span. Fixed spans settle as
/// selected, without a final character-level drag update.
pub fn drag_is_fixed(scope: SelectionScope, key: &str) -> bool {
    state()
        .lock()
        .unwrap()
        .get(&scope)
        .and_then(|s| s.as_ref())
        .is_some_and(|sel| sel.dragging && sel.anchor_key == key && sel.fixed_span)
}

/// Replace the resolved spans + drag head (drag update). Returns true if
/// anything changed (repaint gate). `head_key` is the element under the
/// mouse; it always trails the span resolution in the same frame. A FIXED
/// double/triple-click span is never replaced here — the renderer skips
/// drag updates for it ([`Self::drag_is_fixed`]), and this guard makes the
/// invariant hold even if a stray caller resolves against its position.
pub fn update_drag(
    scope: SelectionScope,
    head_key: &str,
    head_ix: usize,
    spans: Vec<Span>,
) -> bool {
    let mut guard = state().lock().unwrap();
    let Some(sel) = guard.get_mut(&scope).and_then(|s| s.as_mut()) else {
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
pub fn end_drag(scope: SelectionScope, key: &str) -> Option<SelectionSnapshot> {
    let mut guard = state().lock().unwrap();
    let sel = guard.get_mut(&scope).and_then(|s| s.as_mut())?;
    if sel.anchor_key != key || !sel.dragging {
        return None;
    }
    sel.dragging = false;
    if sel.spans.iter().all(|s| s.range.is_empty()) {
        guard.remove(&scope);
        return None;
    }
    Some(SelectionSnapshot {
        head_key: sel.head_key.clone(),
        head_ix: sel.head_ix,
        spans: sel.spans.clone(),
        text: join_spans(&sel.spans),
    })
}

/// Unconditionally drop `scope`'s selection (chat switch, row replacement).
pub fn clear(scope: SelectionScope) {
    state().lock().unwrap().remove(&scope);
}

/// Clear if `key` owns a settled selection (a mouse-down landed outside the
/// owner; the element the down landed IN claims right after). True if cleared.
pub fn clear_if_owner(scope: SelectionScope, key: &str) -> bool {
    let mut guard = state().lock().unwrap();
    if guard
        .get(&scope)
        .and_then(|s| s.as_ref())
        .is_some_and(|s| s.anchor_key == key && !s.dragging)
    {
        guard.remove(&scope);
        return true;
    }
    false
}

/// The wash range for `key` this frame (empty ⇒ nothing to paint).
pub fn wash_range(scope: SelectionScope, key: &str) -> Option<Range<usize>> {
    let guard = state().lock().unwrap();
    let sel = guard.get(&scope)?.as_ref()?;
    sel.spans
        .iter()
        .find(|s| s.key == key && !s.range.is_empty())
        .map(|s| s.range.clone())
}

/// The full selected text (Cmd+C), spans joined in document order. There is
/// exactly ONE active selection at any time — [`begin`]/[`begin_with_span`]
/// clear every other scope, so this returns the LATEST selection no matter
/// which surface (transcript, any diff pane) it came from.
pub fn selected_text() -> Option<String> {
    let guard = state().lock().unwrap();
    let sel = guard.values().find_map(|s| s.as_ref())?;
    if sel.spans.iter().all(|s| s.range.is_empty()) {
        return None;
    }
    Some(join_spans(&sel.spans))
}

fn join_spans(spans: &[Span]) -> String {
    spans
        .iter()
        .filter(|s| !s.range.is_empty() || s.text.is_empty())
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
pub(crate) mod tests;
