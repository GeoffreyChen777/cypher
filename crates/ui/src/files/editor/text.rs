//! Pure text geometry for the editor: line starts, display columns with
//! tabs expanded, reveal scrolling and auto-indent.

use cypher_syntax::HighlightSpan;
use gpui::SharedString;

use super::TAB_WIDTH;

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Byte offset of every line start (always begins with 0) — the same rule
/// `cypher_syntax` uses, so a highlighted document's rows line up 1:1.
pub fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        text.bytes()
            .enumerate()
            .filter_map(|(ix, byte)| (byte == b'\n').then_some(ix + 1)),
    );
    starts
}

/// Display columns of `raw` with tabs expanded to the next [`TAB_WIDTH`] stop
/// (a trailing `\r` is invisible).
pub fn display_columns(raw: &str) -> usize {
    let raw = raw.strip_suffix('\r').unwrap_or(raw);
    raw.chars().fold(0usize, |col, ch| {
        if ch == '\t' {
            col + (TAB_WIDTH - col % TAB_WIDTH)
        } else {
            col + 1
        }
    })
}

/// Raw byte offset within `raw` of display column `col` (clamped to the
/// line end): the character whose columns contain `col` — inside an
/// expanded tab that is the tab itself. Vertical caret moves keep the
/// column, not the byte.
pub fn byte_for_column(raw: &str, col: usize) -> usize {
    let raw = raw.strip_suffix('\r').unwrap_or(raw);
    let mut at = 0usize;
    for (ix, ch) in raw.char_indices() {
        let next = at
            + if ch == '\t' {
                TAB_WIDTH - at % TAB_WIDTH
            } else {
                1
            };
        if col < next {
            return ix;
        }
        at = next;
    }
    raw.len()
}

/// One line as painted: tabs expanded, `\r` dropped, with the display byte
/// offset of every raw byte boundary when the two differ.
pub struct DisplayLine {
    pub text: SharedString,
    pub(super) map: Option<Vec<usize>>,
}

impl DisplayLine {
    pub fn new(raw: &str) -> Self {
        let visible = raw.strip_suffix('\r').unwrap_or(raw);
        if !visible.contains('\t') && visible.len() == raw.len() {
            return Self {
                text: raw.to_string().into(),
                map: None,
            };
        }
        let mut out = String::with_capacity(raw.len() + 8);
        let mut map = Vec::with_capacity(raw.len() + 1);
        let mut col = 0usize;
        for (raw_ix, ch) in visible.char_indices() {
            while map.len() < raw_ix {
                map.push(out.len());
            }
            map.push(out.len());
            if ch == '\t' {
                let n = TAB_WIDTH - col % TAB_WIDTH;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            } else {
                out.push(ch);
                col += 1;
            }
        }
        // One entry per visible raw byte boundary; a stripped `\r` clamps
        // onto the end in `to_display`.
        while map.len() <= visible.len() {
            map.push(out.len());
        }
        Self {
            text: out.into(),
            map: Some(map),
        }
    }

    pub fn to_display(&self, raw: usize) -> usize {
        match &self.map {
            None => raw,
            Some(map) => map[raw.min(map.len() - 1)],
        }
    }

    pub fn to_raw(&self, display: usize) -> usize {
        match &self.map {
            None => display,
            Some(map) => map.partition_point(|d| *d <= display).saturating_sub(1),
        }
    }

    /// Spans (raw byte ranges) re-based onto the display text.
    pub(super) fn shift_spans(&self, spans: &[HighlightSpan]) -> Vec<HighlightSpan> {
        spans
            .iter()
            .map(|span| HighlightSpan {
                range: self.to_display(span.range.start)..self.to_display(span.range.end),
                kind: span.kind,
            })
            .filter(|span| span.range.start < span.range.end)
            .collect()
    }
}

/// Minimally move a scroll offset so `[start, start + extent)` is inside a
/// viewport of `viewport` px over content `content` px long.
pub fn scroll_to_reveal(current: f32, start: f32, extent: f32, content: f32, viewport: f32) -> f32 {
    let mut next = current;
    if start < next {
        next = start;
    } else if start + extent > next + viewport {
        next = start + extent - viewport;
    }
    next.clamp(0.0, (content - viewport).max(0.0))
}

/// The indentation a new line inherits: the current line's leading
/// whitespace, cut at the caret so Enter mid-indent does not double it.
pub fn auto_indent(line: &str, caret_in_line: usize) -> &str {
    let lead = line
        .char_indices()
        .find(|(_, ch)| !matches!(ch, ' ' | '\t'))
        .map(|(ix, _)| ix)
        .unwrap_or(line.len());
    &line[..lead.min(caret_in_line)]
}
