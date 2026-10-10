//! In-chat find (⌘F): the pure matcher plus the paint-side registry the
//! markdown renderer consults when it washes matches under the glyphs.
//!
//! The split mirrors [`crate::markdown::selection`]: this module is the
//! gpui-free state half (unit-tested), the renderer owns geometry and paint.
//!
//! **Why a registry instead of a render option.** A transcript row is a TREE
//! of text elements (paragraphs, list items, table cells, whole fences), and
//! the virtualized list only builds the visible ones. The owning
//! [`Transcript`](crate::transcript) counts matches per ROW (cheap, memoized
//! per row version) and publishes the query plus the ACTIVE match as a
//! `(row id, ordinal within the row)` pair; each text element then resolves
//! its own byte ranges as it is built. Element ordinals come from a counter
//! the row opens with [`begin_row`] and every element of that row consumes in
//! build order — which IS document order, the same invariant the selection
//! registry already relies on. So the ordinals the painter assigns and the
//! ordinals the transcript's per-row counts imply agree without either side
//! materializing the other's text.
//!
//! Surfaces that render markdown outside a transcript (the appearance
//! preview, the Files Markdown view) never call [`begin_row`], so they never
//! highlight — no scope plumbing needed to keep them out.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

use crate::markdown::selection::SelectionScope;

// ---------------------------------------------------------------------------
// Matching (pure)
// ---------------------------------------------------------------------------

/// Case-insensitive, non-overlapping matches of `needle` in `haystack`, as
/// byte ranges into `haystack`.
///
/// Folding is Unicode `to_lowercase` on both sides, compared by folded CHAR,
/// so "Straße" matches "STRASSE"-style expansions the way the text actually
/// reads. A match that would end inside a haystack character whose lowercase
/// expands to several characters is reported to that character's boundary —
/// a range can only ever cover whole glyphs.
pub fn match_ranges(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    scan(haystack, needle, |range| out.push(range));
    out
}

/// How many times `needle` occurs in `haystack` ([`match_ranges`] without the
/// allocation — this one runs over the WHOLE transcript on every query edit).
pub fn count_matches(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    scan(haystack, needle, |_| count += 1);
    count
}

/// The one scanner both entry points share, so a count and a highlight can
/// never disagree about what matched.
fn scan(haystack: &str, needle: &str, mut found: impl FnMut(Range<usize>)) {
    let folded = fold(needle);
    if folded.is_empty() {
        return;
    }
    // All-ASCII on both sides (ordinary prose, code, and queries) folds
    // byte-for-byte, so the scan can stay in bytes — every offset it produces
    // is an ASCII byte and therefore a char boundary. Anything else takes the
    // general path, where a haystack character may fold to several characters.
    if haystack.is_ascii() && folded.iter().all(char::is_ascii) {
        let hay = haystack.as_bytes();
        let want: Vec<u8> = folded.iter().map(|ch| *ch as u8).collect();
        let mut at = 0;
        while at + want.len() <= hay.len() {
            if hay[at..at + want.len()]
                .iter()
                .zip(&want)
                .all(|(byte, wanted)| byte.to_ascii_lowercase() == *wanted)
            {
                found(at..at + want.len());
                at += want.len();
            } else {
                at += 1;
            }
        }
        return;
    }
    let mut at = 0;
    while at < haystack.len() {
        match match_at(haystack, at, &folded) {
            Some(end) => {
                found(at..end);
                at = end;
            }
            None => {
                at += haystack[at..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
}

fn fold(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// The end offset of `folded`'s match anchored at `at`, or `None`.
fn match_at(haystack: &str, at: usize, folded: &[char]) -> Option<usize> {
    let mut want = 0;
    let mut end = at;
    for ch in haystack[at..].chars() {
        for lowered in ch.to_lowercase() {
            if want == folded.len() {
                break;
            }
            if folded[want] != lowered {
                return None;
            }
            want += 1;
        }
        end += ch.len_utf8();
        if want == folded.len() {
            return Some(end);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Paint registry
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ScopeFind {
    /// The live query. Never empty while an entry exists.
    query: String,
    /// `(row id, ordinal within that row)` of the ACTIVE match.
    active: Option<(String, usize)>,
    /// Per-row match ordinal counters, opened by [`begin_row`] and consumed in
    /// build order by that row's text elements.
    ordinals: HashMap<String, usize>,
}

thread_local! {
    static FIND: RefCell<HashMap<SelectionScope, ScopeFind>> =
        RefCell::new(HashMap::new());
}

/// Publish a surface's live find state. An empty `query` clears the scope, so
/// closing the find bar and emptying its field are the same code path.
pub fn publish(scope: SelectionScope, query: &str, active: Option<(String, usize)>) {
    FIND.with(|find| {
        let mut find = find.borrow_mut();
        if query.is_empty() {
            find.remove(&scope);
            return;
        }
        let entry = find.entry(scope).or_default();
        if entry.query != query {
            entry.query.clear();
            entry.query.push_str(query);
            // Stale counters belong to the previous query's element walk.
            entry.ordinals.clear();
        }
        entry.active = active;
    });
}

/// Forget everything about `scope` (find closed, surface dropped).
pub fn clear(scope: SelectionScope) {
    FIND.with(|find| {
        find.borrow_mut().remove(&scope);
    });
}

/// Open `row`'s ordinal counter — call once per row, before building its text
/// elements. Rows that are never opened never highlight.
pub fn begin_row(scope: SelectionScope, row: &str) {
    FIND.with(|find| {
        if let Some(entry) = find.borrow_mut().get_mut(&scope) {
            entry.ordinals.insert(row.to_owned(), 0);
        }
    });
}

/// One text element's matches: byte ranges into `text`, each paired with
/// whether it is the surface's ACTIVE match. Consumes the row's ordinals, so
/// call exactly once per element per build pass.
pub fn element_matches(scope: SelectionScope, row: &str, text: &str) -> Vec<(Range<usize>, bool)> {
    FIND.with(|find| {
        let mut find = find.borrow_mut();
        let Some(ScopeFind {
            query,
            active,
            ordinals,
        }) = find.get_mut(&scope)
        else {
            return Vec::new();
        };
        let Some(next) = ordinals.get_mut(row) else {
            return Vec::new();
        };
        let ranges = match_ranges(text, query);
        if ranges.is_empty() {
            return Vec::new();
        }
        let base = *next;
        *next = base + ranges.len();
        let active = active
            .as_ref()
            .filter(|(active_row, _)| active_row == row)
            .map(|(_, ordinal)| *ordinal);
        ranges
            .into_iter()
            .enumerate()
            .map(|(ix, range)| (range, active == Some(base + ix)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_case_insensitive_and_non_overlapping() {
        assert_eq!(
            match_ranges("Hello hello HELLO", "hello"),
            [0..5, 6..11, 12..17]
        );
        // "aa" in "aaaa" is two matches, not three — the scan resumes past
        // each hit (browser find semantics).
        assert_eq!(match_ranges("aaaa", "aa"), [0..2, 2..4]);
        assert_eq!(count_matches("aaaa", "aa"), 2);
    }

    #[test]
    fn empty_query_never_matches() {
        assert!(match_ranges("anything", "").is_empty());
        assert_eq!(count_matches("anything", ""), 0);
    }

    #[test]
    fn ranges_are_byte_offsets_into_the_original_text() {
        // Multibyte prefix: the range must index the ORIGINAL string, not a
        // lowercased copy (slicing by a folded offset would panic here).
        let text = "日本語 Cypher 日本語 cypher";
        let ranges = match_ranges(text, "CYPHER");
        assert_eq!(ranges.len(), 2);
        for range in ranges {
            assert!(text[range].eq_ignore_ascii_case("cypher"));
        }
    }

    #[test]
    fn folding_handles_multi_char_lowercase() {
        // 'İ' (U+0130) lowercases to two chars; the fold compares both.
        assert_eq!(count_matches("İstanbul", "i\u{307}stanbul"), 1);
    }

    #[test]
    fn the_ascii_fast_path_agrees_with_the_general_one() {
        // Same needle against an ASCII haystack (fast path) and the same
        // haystack carrying one non-ASCII character (general path).
        for needle in ["x", "ab", "AB", "abab"] {
            for haystack in ["abab x ABAB", "xx", "", "a"] {
                let ascii = match_ranges(haystack, needle);
                let general = match_ranges(&format!("é{haystack}"), needle);
                assert_eq!(ascii.len(), general.len(), "{needle:?} in {haystack:?}");
                for (fast, slow) in ascii.iter().zip(&general) {
                    // The general haystack is offset by the 2-byte 'é'.
                    assert_eq!(fast.start + 2, slow.start);
                    assert_eq!(fast.end + 2, slow.end);
                }
            }
        }
    }

    #[test]
    fn unopened_rows_do_not_highlight() {
        let scope = crate::markdown::selection::next_side_chat_scope();
        publish(scope, "cypher", None);
        assert!(element_matches(scope, "row-1", "cypher").is_empty());
        begin_row(scope, "row-1");
        assert_eq!(element_matches(scope, "row-1", "cypher"), [(0..6, false)]);
        clear(scope);
    }

    #[test]
    fn ordinals_accumulate_across_a_rows_elements() {
        let scope = crate::markdown::selection::next_side_chat_scope();
        // The 3rd match of the row is active — it sits in the SECOND element.
        publish(scope, "x", Some(("row".to_owned(), 2)));
        begin_row(scope, "row");
        let first = element_matches(scope, "row", "x x");
        let second = element_matches(scope, "row", "x x");
        assert_eq!(first, [(0..1, false), (2..3, false)]);
        assert_eq!(second, [(0..1, true), (2..3, false)]);
        // Re-opening the row rewinds the counter for the next build pass.
        begin_row(scope, "row");
        assert_eq!(
            element_matches(scope, "row", "x x"),
            [(0..1, false), (2..3, false)]
        );
        clear(scope);
    }

    #[test]
    fn an_empty_query_clears_the_scope() {
        let scope = crate::markdown::selection::next_side_chat_scope();
        publish(scope, "cypher", None);
        begin_row(scope, "row");
        publish(scope, "", None);
        assert!(element_matches(scope, "row", "cypher").is_empty());
    }
}
