//! The Files surface's editor: one file's text as UNWRAPPED lines, shaped
//! only where visible, colored by `cypher-syntax`, edited through gpui's IME
//! input handler — the composer's proven text-input recipe minus wrapping,
//! mentions and auto-grow, plus a line-number gutter, horizontal scroll, and
//! per-line virtualization so a 256 KiB source stays cheap to paint.
//!
//! Byte offsets into `content` are the only coordinate system for the caret
//! and selection; lines are found by binary search over `line_starts`. Tabs
//! expand to the next 4-column stop for display only ([`DisplayLine`] maps
//! raw ↔ display byte offsets per line), so caret math never sees them.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Task, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use cypher_syntax::{HighlightRequest, HighlightSpan, HighlightedDocument, LanguageId};

use crate::markdown::render;
use crate::theme::Theme;

pub const TEXT_SIZE: f32 = 12.0;
pub const LINE_HEIGHT: f32 = 20.0;
/// Air on both sides of the line-number column.
const GUTTER_PAD: f32 = 10.0;
/// Space between the gutter's right edge and the first glyph.
const TEXT_PAD_LEFT: f32 = 6.0;
/// Room past the longest line so the caret at its end never hugs the edge.
const TEXT_PAD_RIGHT: f32 = 48.0;
const TAB_WIDTH: usize = 4;
const UNDO_LIMIT: usize = 100;
const UNDO_COALESCE: Duration = Duration::from_millis(700);
/// Edits re-highlight after this idle window (a keystroke never parses).
const HIGHLIGHT_DEBOUNCE: Duration = Duration::from_millis(120);
const CARET_BLINK_MS: u64 = 500;

pub const KEY_CONTEXT: &str = "FilesEditor";

actions!(
    files_editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        PageUp,
        PageDown,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Newline,
        Indent,
        Save,
    ]
);

/// Bind the editor keymap, scoped to [`KEY_CONTEXT`] so nothing leaks into
/// the composer or the palettes. Re-run whenever the app clears its
/// bindings (the shortcuts page re-applies the keymap).
pub fn init(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT);
    let (cmd, word) = if cfg!(target_os = "macos") {
        ("cmd", "alt")
    } else {
        ("ctrl", "ctrl")
    };
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        KeyBinding::new("pageup", PageUp, ctx),
        KeyBinding::new("pagedown", PageDown, ctx),
        KeyBinding::new("enter", Newline, ctx),
        KeyBinding::new("tab", Indent, ctx),
        KeyBinding::new(&format!("{word}-left"), WordLeft, ctx),
        KeyBinding::new(&format!("{word}-right"), WordRight, ctx),
        KeyBinding::new(&format!("shift-{word}-left"), SelectWordLeft, ctx),
        KeyBinding::new(&format!("shift-{word}-right"), SelectWordRight, ctx),
        KeyBinding::new(&format!("{word}-backspace"), DeleteWordLeft, ctx),
        KeyBinding::new(&format!("{word}-delete"), DeleteWordRight, ctx),
        KeyBinding::new(&format!("{cmd}-a"), SelectAll, ctx),
        KeyBinding::new(&format!("{cmd}-c"), Copy, ctx),
        KeyBinding::new(&format!("{cmd}-x"), Cut, ctx),
        KeyBinding::new(&format!("{cmd}-v"), Paste, ctx),
        KeyBinding::new(&format!("{cmd}-z"), Undo, ctx),
        KeyBinding::new(&format!("shift-{cmd}-z"), Redo, ctx),
        KeyBinding::new(&format!("{cmd}-s"), Save, ctx),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-left", Home, ctx),
            KeyBinding::new("cmd-right", End, ctx),
            KeyBinding::new("cmd-up", DocStart, ctx),
            KeyBinding::new("cmd-down", DocEnd, ctx),
            KeyBinding::new("shift-cmd-left", SelectHome, ctx),
            KeyBinding::new("shift-cmd-right", SelectEnd, ctx),
            KeyBinding::new("shift-cmd-up", SelectDocStart, ctx),
            KeyBinding::new("shift-cmd-down", SelectDocEnd, ctx),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-home", DocStart, ctx),
            KeyBinding::new("ctrl-end", DocEnd, ctx),
            KeyBinding::new("shift-ctrl-home", SelectDocStart, ctx),
            KeyBinding::new("shift-ctrl-end", SelectDocEnd, ctx),
            KeyBinding::new("ctrl-y", Redo, ctx),
        ]);
    }
    cx.bind_keys(bindings);
}

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
    map: Option<Vec<usize>>,
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
    fn shift_spans(&self, spans: &[HighlightSpan]) -> Vec<HighlightSpan> {
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

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeEditorEvent {
    /// The text changed (typing, paste, undo…).
    Edited,
    /// ⌘S / Ctrl+S — the host decides what saving means.
    Save,
}

struct Snapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
}

pub struct CodeEditor {
    focus_handle: FocusHandle,
    path: String,
    language: Option<LanguageId>,
    content: String,
    line_starts: Vec<usize>,
    /// Widest line in display columns — the horizontal scroll extent.
    max_columns: usize,
    read_only: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    is_selecting: bool,
    scroll_top: f32,
    scroll_left: f32,
    /// Keeps the caret in view through edits and moves; a wheel scroll
    /// pauses it until the next caret move.
    follow_cursor: bool,
    // -- measured (written during prepaint) --
    text_bounds: Option<Bounds<Pixels>>,
    char_width: f32,
    /// Shaped lines of the last prepaint: `(line index, display, shaped)`.
    visible: Vec<(usize, DisplayLine, ShapedLine)>,
    // -- highlighting --
    highlight: Option<Arc<HighlightedDocument>>,
    highlight_gen: u64,
    highlight_task: Option<Task<()>>,
    // -- caret blink --
    blink_anchor: Instant,
    blink_task: Option<Task<()>>,
    // -- undo history --
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    last_edit: Option<(EditKind, usize, Instant)>,
}

impl EventEmitter<CodeEditorEvent> for CodeEditor {}

impl Focusable for CodeEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CodeEditor {
    pub fn new(path: &str, cx: &mut Context<Self>) -> Self {
        let language = cypher_syntax::language_for_path(path)
            .filter(|lang| cypher_syntax::supports_language(*lang));
        Self {
            focus_handle: cx.focus_handle(),
            path: path.to_string(),
            language,
            content: String::new(),
            line_starts: vec![0],
            max_columns: 0,
            read_only: true,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            is_selecting: false,
            scroll_top: 0.0,
            scroll_left: 0.0,
            follow_cursor: true,
            text_bounds: None,
            char_width: 0.0,
            visible: Vec::new(),
            highlight: None,
            highlight_gen: 0,
            highlight_task: None,
            blink_anchor: Instant::now(),
            blink_task: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
        }
    }

    /// Load (or reload) the file's text. Resets caret, scroll and history —
    /// this is a fresh document, not an edit.
    pub fn set_content(&mut self, text: String, read_only: bool, cx: &mut Context<Self>) {
        self.content = text;
        self.read_only = read_only;
        self.rebuild_lines();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_top = 0.0;
        self.scroll_left = 0.0;
        self.follow_cursor = true;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        self.highlight = None;
        self.schedule_highlight(Duration::ZERO, cx);
        cx.notify();
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn language(&self) -> Option<LanguageId> {
        self.language
    }

    /// `(line, column)` of the caret, 1-based, for a status readout.
    pub fn caret_position(&self) -> (usize, usize) {
        let offset = self.cursor_offset();
        let line = self.line_of(offset);
        let column = display_columns(&self.content[self.line_starts[line]..offset]);
        (line + 1, column + 1)
    }

    // ---- lines ----

    fn rebuild_lines(&mut self) {
        self.line_starts = line_starts(&self.content);
        self.max_columns = self
            .content
            .split('\n')
            .map(display_columns)
            .max()
            .unwrap_or(0);
    }

    fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    fn line_of(&self, offset: usize) -> usize {
        self.line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    }

    /// The line's editable bytes: everything before its terminator, which
    /// is `\n` or `\r\n` (the caret never sits between the two — they are
    /// one grapheme, and typing there would corrupt a CRLF file).
    fn line_range(&self, ix: usize) -> Range<usize> {
        let start = self.line_starts[ix];
        let mut end = self
            .line_starts
            .get(ix + 1)
            .map(|next| next - 1)
            .unwrap_or(self.content.len());
        if end > start && self.content.as_bytes()[end - 1] == b'\r' && end < self.content.len() {
            end -= 1;
        }
        start..end
    }

    /// `cypher_syntax` omits the empty line after a trailing newline, so
    /// its document is one row short of [`Self::line_count`] for most
    /// files; that last row is empty either way. Anything else is a stale
    /// document from before an edit — paint plain until the re-parse lands.
    fn spans_for_line(&self, ix: usize) -> &[HighlightSpan] {
        match &self.highlight {
            Some(doc)
                if doc.lines.len() == self.line_count()
                    || (doc.lines.len() + 1 == self.line_count()
                        && self.content.ends_with('\n')) =>
            {
                doc.lines.get(ix).map(Vec::as_slice).unwrap_or(&[])
            }
            _ => &[],
        }
    }

    // ---- highlighting ----

    fn schedule_highlight(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if self.language.is_none() {
            self.highlight = None;
            self.highlight_task = None;
            return;
        }
        self.highlight_gen += 1;
        let generation = self.highlight_gen;
        let source = self.content.clone();
        let path = self.path.clone();
        self.highlight_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let document = cx
                .background_executor()
                .spawn(async move {
                    cypher_syntax::highlight(HighlightRequest {
                        source: &source,
                        path: Some(&path),
                        fence_tag: None,
                    })
                    .ok()
                    .map(Arc::new)
                })
                .await;
            this.update(cx, |editor, cx| {
                if editor.highlight_gen == generation {
                    editor.highlight = document;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    // ---- caret blink ----

    fn reset_blink(&mut self) {
        self.blink_anchor = Instant::now();
    }

    fn caret_shown(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let focused = self.focus_handle.is_focused(window);
        if !focused || !window.is_window_active() {
            self.blink_task = None;
            return false;
        }
        if self.blink_task.is_none() {
            self.blink_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(CARET_BLINK_MS))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
        let elapsed = self.blink_anchor.elapsed().as_millis() as u64;
        (elapsed / CARET_BLINK_MS).is_multiple_of(2)
    }

    // ---- undo history ----

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    /// Called with the range about to be replaced, BEFORE the content
    /// changes, so the pushed snapshot is the pre-edit state. Single
    /// characters typed in a row merge into one undo step.
    fn record_edit(&mut self, range: &Range<usize>, new_text: &str) {
        let kind = if new_text.is_empty() {
            EditKind::Delete
        } else {
            EditKind::Insert
        };
        let mergeable = match (kind, &self.last_edit) {
            (EditKind::Insert, Some((EditKind::Insert, at, when))) => {
                range.is_empty()
                    && range.start == *at
                    && new_text.chars().count() == 1
                    && !new_text.starts_with(['\n', ' ', '\t'])
                    && when.elapsed() < UNDO_COALESCE
            }
            (EditKind::Delete, Some((EditKind::Delete, at, when))) => {
                range.end == *at && when.elapsed() < UNDO_COALESCE
            }
            _ => false,
        };
        if !mergeable {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
        }
        self.redo_stack.clear();
        let tail = match kind {
            EditKind::Insert => range.start + new_text.len(),
            EditKind::Delete => range.start,
        };
        self.last_edit = Some((kind, tail, Instant::now()));
    }

    fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.rebuild_lines();
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = None;
        self.follow_cursor = true;
        self.last_edit = None;
        self.reset_blink();
        self.schedule_highlight(HIGHLIGHT_DEBOUNCE, cx);
        cx.emit(CodeEditorEvent::Edited);
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(previous, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(next, cx);
    }

    // ---- editing ops ----

    pub fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn clamp_boundary(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp_boundary(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.last_edit = None;
        self.reset_blink();
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp_boundary(offset);
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.follow_cursor = true;
        self.reset_blink();
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content[..offset]
            .grapheme_indices(true)
            .next_back()
            .map(|(ix, _)| ix)
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content[offset..]
            .graphemes(true)
            .next()
            .map(|g| offset + g.len())
            .unwrap_or(self.content.len())
    }

    fn previous_word_boundary(&self, offset: usize) -> usize {
        self.content[..offset]
            .split_word_bound_indices()
            .rev()
            .find_map(|(ix, word)| (!word.trim().is_empty()).then_some(ix))
            .unwrap_or(0)
    }

    fn next_word_boundary(&self, offset: usize) -> usize {
        self.content[offset..]
            .split_word_bound_indices()
            .find_map(|(ix, word)| (!word.trim().is_empty()).then_some(offset + ix + word.len()))
            .unwrap_or(self.content.len())
    }

    /// The offset `delta` lines away from the caret at the same display
    /// column, clamped to the document edges.
    fn vertical_target(&self, delta: isize) -> usize {
        let cursor = self.cursor_offset();
        let line = self.line_of(cursor);
        let column = display_columns(&self.content[self.line_starts[line]..cursor]);
        let target = line as isize + delta;
        if target < 0 {
            return 0;
        }
        let target = target as usize;
        if target >= self.line_count() {
            return self.content.len();
        }
        let range = self.line_range(target);
        range.start + byte_for_column(&self.content[range.clone()], column)
    }

    fn page_lines(&self) -> isize {
        let height = self
            .text_bounds
            .map(|b| f32::from(b.size.height))
            .unwrap_or(LINE_HEIGHT * 10.0);
        ((height / LINE_HEIGHT).floor() as isize - 1).max(1)
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let start = self.previous_boundary(self.cursor_offset());
            self.selected_range = start..self.cursor_offset();
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let end = self.next_boundary(self.cursor_offset());
            self.selected_range = self.cursor_offset()..end;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            let start = self.previous_word_boundary(self.cursor_offset());
            self.selected_range = start..self.cursor_offset();
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_right(
        &mut self,
        _: &DeleteWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            let end = self.next_word_boundary(self.cursor_offset());
            self.selected_range = self.cursor_offset()..end;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(-1), cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(1), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_target(-1), cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_target(1), cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(-self.page_lines()), cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(self.page_lines()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word_boundary(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word_boundary(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word_boundary(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.move_to(line.start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.move_to(line.end, cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.select_to(line.start, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.select_to(line.end, cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.selected_range.start;
        let line = self.line_range(self.line_of(cursor));
        let indent = auto_indent(&self.content[line.clone()], cursor - line.start).to_string();
        self.replace_text_in_range(None, &format!("\n{indent}"), window, cx);
    }

    /// Tab inserts a real tab on tab-indented lines, else spaces up to the
    /// next stop — never shifts selections (kept deliberately small).
    fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.selected_range.start;
        let line = self.line_range(self.line_of(cursor));
        let text = if self.content[line.clone()].starts_with('\t') {
            "\t".to_string()
        } else {
            let column = display_columns(&self.content[line.start..cursor]);
            " ".repeat(TAB_WIDTH - column % TAB_WIDTH)
        };
        self.replace_text_in_range(None, &text, window, cx);
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(CodeEditorEvent::Save);
    }

    // ---- geometry ----

    fn content_height(&self) -> f32 {
        self.line_count() as f32 * LINE_HEIGHT
    }

    fn content_width(&self) -> f32 {
        self.max_columns as f32 * self.char_width + TEXT_PAD_LEFT + TEXT_PAD_RIGHT
    }

    /// Text-area-local point (unscrolled) of a byte offset, when its line
    /// was shaped this frame.
    fn point_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        let line = self.line_of(index);
        let (_, display, shaped) = self.visible.iter().find(|(ix, _, _)| *ix == line)?;
        let local = display.to_display(index - self.line_starts[line]);
        Some(point(
            px(TEXT_PAD_LEFT) + shaped.x_for_index(local),
            px(line as f32 * LINE_HEIGHT),
        ))
    }

    /// Byte offset closest to a window point. Lines outside the shaped
    /// window clamp to their start (above) or end (below).
    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.text_bounds else {
            return 0;
        };
        let y = f32::from(position.y - bounds.top()) + self.scroll_top;
        let line = ((y / LINE_HEIGHT).floor().max(0.0) as usize).min(self.line_count() - 1);
        let x = px(f32::from(position.x - bounds.left()) + self.scroll_left - TEXT_PAD_LEFT);
        let range = self.line_range(line);
        match self.visible.iter().find(|(ix, _, _)| *ix == line) {
            Some((_, display, shaped)) => {
                let local = shaped.closest_index_for_x(x.max(px(0.0)));
                range.start + display.to_raw(local).min(range.len())
            }
            None if self
                .visible
                .first()
                .is_some_and(|(first, _, _)| line < *first) =>
            {
                range.start
            }
            None => range.end,
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let index = self.index_for_point(event.position);
        match event.click_count {
            2 => {
                // The word under the pointer: probe one grapheme in so a
                // click on a word's first letter picks THAT word.
                let probe = if self.content[index..]
                    .chars()
                    .next()
                    .is_some_and(|ch| !ch.is_whitespace())
                {
                    self.next_boundary(index)
                } else {
                    index
                };
                let start = self.previous_word_boundary(probe);
                let end = self.next_word_boundary(start).max(index);
                self.move_to(start, cx);
                self.select_to(end, cx);
            }
            n if n >= 3 => {
                let line = self.line_range(self.line_of(index));
                self.move_to(line.start, cx);
                self.select_to(line.end, cx);
            }
            _ => {
                self.is_selecting = true;
                if event.modifiers.shift {
                    self.select_to(index, cx);
                } else {
                    self.move_to(index, cx);
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.is_selecting {
            let index = self.index_for_point(event.position);
            self.select_to(index, cx);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.text_bounds else {
            return;
        };
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
        let max_top = (self.content_height() - f32::from(bounds.size.height)).max(0.0);
        let max_left = (self.content_width() - f32::from(bounds.size.width)).max(0.0);
        let top = (self.scroll_top - f32::from(delta.y)).clamp(0.0, max_top);
        let left = (self.scroll_left - f32::from(delta.x)).clamp(0.0, max_left);
        if top == self.scroll_top && left == self.scroll_left {
            return;
        }
        self.scroll_top = top;
        self.scroll_left = left;
        self.follow_cursor = false;
        cx.stop_propagation();
        cx.notify();
    }

    // ---- utf16 mapping (IME) ----

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    // ---- per-frame layout (called from the element's prepaint) ----

    fn shape_line(
        &self,
        ix: usize,
        theme: &Theme,
        mono: &gpui::Font,
        window: &Window,
    ) -> (DisplayLine, ShapedLine) {
        let range = self.line_range(ix);
        let raw = &self.content[range.clone()];
        let display = DisplayLine::new(raw);
        let text_color = theme.text.opacity(0.92);
        let runs = match &self.marked_range {
            // An IME composition on this line: plain text with the
            // composed segment underlined (the platform convention).
            Some(marked) if marked.start < range.end && marked.end > range.start => {
                let start = display.to_display(marked.start.max(range.start) - range.start);
                let end = display.to_display(marked.end.min(range.end) - range.start);
                let run = |len: usize, underline: bool| TextRun {
                    len,
                    font: mono.clone(),
                    color: text_color,
                    background_color: None,
                    underline: underline.then_some(UnderlineStyle {
                        color: Some(text_color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                vec![
                    run(start, false),
                    run(end.saturating_sub(start), true),
                    run(display.text.len().saturating_sub(end), false),
                ]
                .into_iter()
                .filter(|r| r.len > 0)
                .collect()
            }
            _ => {
                let spans = self.spans_for_line(ix);
                let shifted;
                let spans: &[HighlightSpan] = if display.map.is_some() {
                    shifted = display.shift_spans(spans);
                    &shifted
                } else {
                    spans
                };
                render::runs_for_syntax_line_with_plain(
                    &display.text,
                    spans,
                    mono,
                    text_color,
                    theme,
                )
            }
        };
        let shaped =
            window
                .text_system()
                .shape_line(display.text.clone(), px(TEXT_SIZE), &runs, None);
        (display, shaped)
    }

    /// Measure, follow the caret, and shape the visible window. Returns the
    /// gutter width so the element can split its bounds.
    fn layout(&mut self, bounds: Bounds<Pixels>, theme: &Theme, window: &Window) -> f32 {
        let mono = theme.mono();
        if self.char_width <= 0.0 {
            let run = TextRun {
                len: 1,
                font: mono.clone(),
                color: theme.text,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            self.char_width = f32::from(
                window
                    .text_system()
                    .shape_line("0".into(), px(TEXT_SIZE), &[run], None)
                    .width(),
            )
            .max(1.0);
        }
        let digits = self.line_count().to_string().len().max(2);
        let gutter = digits as f32 * self.char_width + 2.0 * GUTTER_PAD;
        let text_bounds = Bounds::new(
            point(bounds.left() + px(gutter), bounds.top()),
            size(
                (bounds.size.width - px(gutter)).max(px(0.0)),
                bounds.size.height,
            ),
        );
        self.text_bounds = Some(text_bounds);
        let height = f32::from(text_bounds.size.height);
        let width = f32::from(text_bounds.size.width);

        let cursor = self.cursor_offset();
        let cursor_line = self.line_of(cursor);
        if self.follow_cursor {
            self.scroll_top = scroll_to_reveal(
                self.scroll_top,
                cursor_line as f32 * LINE_HEIGHT,
                LINE_HEIGHT,
                self.content_height(),
                height,
            );
        }
        self.scroll_top = self
            .scroll_top
            .clamp(0.0, (self.content_height() - height).max(0.0));

        let first = (self.scroll_top / LINE_HEIGHT).floor() as usize;
        let last =
            (((self.scroll_top + height) / LINE_HEIGHT).ceil() as usize + 1).min(self.line_count());
        self.visible = (first.min(self.line_count())..last)
            .map(|ix| {
                let (display, shaped) = self.shape_line(ix, theme, &mono, window);
                (ix, display, shaped)
            })
            .collect();

        if self.follow_cursor
            && let Some(p) = self.point_for_index(cursor)
        {
            self.scroll_left = scroll_to_reveal(
                self.scroll_left,
                f32::from(p.x) - TEXT_PAD_LEFT,
                TEXT_PAD_LEFT + 2.0,
                self.content_width(),
                width,
            );
        }
        self.scroll_left = self
            .scroll_left
            .clamp(0.0, (self.content_width() - width).max(0.0));
        gutter
    }
}

impl EntityInputHandler for CodeEditor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content.get(range)?.to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        if self.read_only {
            // Keep the caret where a keystroke would have landed so the
            // read-only banner is the only difference the user sees.
            self.marked_range = None;
            self.selected_range = range.start..range.start;
            self.selection_reversed = false;
            cx.notify();
            return;
        }
        if self.marked_range.is_none() {
            self.record_edit(&range, new_text);
        }
        self.content.replace_range(range.clone(), new_text);
        self.rebuild_lines();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.follow_cursor = true;
        self.reset_blink();
        self.schedule_highlight(HIGHLIGHT_DEBOUNCE, cx);
        cx.emit(CodeEditorEvent::Edited);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        if self.marked_range.is_none() {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
            self.last_edit = None;
        }
        self.content.replace_range(range.clone(), new_text);
        self.rebuild_lines();
        self.marked_range = if new_text.is_empty() {
            None
        } else {
            Some(range.start..range.start + new_text.len())
        };
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.reset_blink();
        self.schedule_highlight(HIGHLIGHT_DEBOUNCE, cx);
        cx.emit(CodeEditorEvent::Edited);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let text_bounds = self.text_bounds?;
        let start = self.point_for_index(range.start).unwrap_or_else(|| {
            point(
                px(TEXT_PAD_LEFT),
                px(self.line_of(range.start) as f32 * LINE_HEIGHT),
            )
        });
        Some(Bounds::new(
            point(
                text_bounds.left() + start.x - px(self.scroll_left),
                text_bounds.top() + start.y - px(self.scroll_top),
            ),
            size(px(2.0), px(LINE_HEIGHT)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point_in_window: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_for_point(point_in_window);
        Some(self.offset_to_utf16(index))
    }
}

// ---------------------------------------------------------------------------
// Element
// ---------------------------------------------------------------------------

struct EditorElement {
    editor: Entity<CodeEditor>,
}

struct EditorPrepaint {
    gutter_width: f32,
    /// `(shaped number, origin)` per visible line.
    numbers: Vec<(ShapedLine, Point<Pixels>)>,
    current_line: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = EditorPrepaint;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = Theme::of(cx).clone();
        let gutter_width = self
            .editor
            .update(cx, |editor, _| editor.layout(bounds, &theme, window));
        let editor = self.editor.read(cx);
        let Some(text_bounds) = editor.text_bounds else {
            return EditorPrepaint {
                gutter_width,
                numbers: Vec::new(),
                current_line: None,
                selection: Vec::new(),
                cursor: None,
            };
        };
        let origin = point(
            text_bounds.left() - px(editor.scroll_left),
            text_bounds.top() - px(editor.scroll_top),
        );
        let lh = px(LINE_HEIGHT);
        let mono = theme.mono();
        let number_color = theme
            .regions
            .git_line_number
            .unwrap_or(theme.text_faint.opacity(0.8));
        let cursor_line = editor.line_of(editor.cursor_offset());
        let numbers = editor
            .visible
            .iter()
            .map(|(ix, _, _)| {
                let text: SharedString = (ix + 1).to_string().into();
                let run = TextRun {
                    len: text.len(),
                    font: mono.clone(),
                    color: if *ix == cursor_line {
                        theme.text_muted
                    } else {
                        number_color
                    },
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped = window
                    .text_system()
                    .shape_line(text, px(TEXT_SIZE), &[run], None);
                let x = bounds.left() + px(gutter_width - GUTTER_PAD) - shaped.width();
                let y = bounds.top() + px(*ix as f32 * LINE_HEIGHT - editor.scroll_top);
                (shaped, point(x, y))
            })
            .collect();

        let current_line = Some(fill(
            Bounds::new(
                point(
                    bounds.left(),
                    bounds.top() + px(cursor_line as f32 * LINE_HEIGHT - editor.scroll_top),
                ),
                size(bounds.size.width, lh),
            ),
            crate::theme::wash(0.035),
        ));

        let mut selection = Vec::new();
        let mut cursor = None;
        if editor.selected_range.is_empty() {
            if let Some(p) = editor.point_for_index(editor.cursor_offset()) {
                cursor = Some(fill(
                    Bounds::new(point(origin.x + p.x, origin.y + p.y), size(px(2.0), lh)),
                    theme.caret,
                ));
            }
        } else {
            // One wash per visible line the selection touches; the row's
            // right edge is the text area's, past the scrolled content.
            let sel = editor.selected_range.clone();
            let far_right = text_bounds.right();
            for (ix, display, shaped) in &editor.visible {
                let range = editor.line_range(*ix);
                if sel.end < range.start || sel.start > range.end {
                    continue;
                }
                let y = origin.y + px(*ix as f32 * LINE_HEIGHT);
                let start_x = if sel.start <= range.start {
                    origin.x + px(TEXT_PAD_LEFT)
                } else {
                    origin.x
                        + px(TEXT_PAD_LEFT)
                        + shaped.x_for_index(display.to_display(sel.start - range.start))
                };
                let end_x = if sel.end > range.end {
                    far_right
                } else {
                    origin.x
                        + px(TEXT_PAD_LEFT)
                        + shaped.x_for_index(display.to_display(sel.end - range.start))
                };
                let end_x = if sel.end > range.end {
                    end_x
                } else {
                    end_x.max(start_x + px(2.0))
                };
                if end_x > start_x {
                    selection.push(fill(
                        Bounds::from_corners(point(start_x, y), point(end_x, y + lh)),
                        theme.selection,
                    ));
                }
            }
        }
        EditorPrepaint {
            gutter_width,
            numbers,
            current_line,
            selection,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        let editor = self.editor.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                editor.update(cx, |editor, cx| editor.on_mouse_move(event, cx));
            }
        });

        let (visible, text_bounds, scroll_top, scroll_left) = {
            let editor = self.editor.read(cx);
            (
                editor
                    .visible
                    .iter()
                    .map(|(ix, _, shaped)| (*ix, shaped.clone()))
                    .collect::<Vec<_>>(),
                editor.text_bounds,
                editor.scroll_top,
                editor.scroll_left,
            )
        };
        let Some(text_bounds) = text_bounds else {
            return;
        };
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            if let Some(quad) = prepaint.current_line.take() {
                window.paint_quad(quad);
            }
            for (shaped, origin) in prepaint.numbers.drain(..) {
                let _ = shaped.paint(
                    origin,
                    px(LINE_HEIGHT),
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        });
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: text_bounds,
            }),
            |window| {
                for quad in prepaint.selection.drain(..) {
                    window.paint_quad(quad);
                }
                let x = text_bounds.left() + px(TEXT_PAD_LEFT - scroll_left);
                for (ix, shaped) in &visible {
                    let y = text_bounds.top() + px(*ix as f32 * LINE_HEIGHT - scroll_top);
                    let _ = shaped.paint(
                        point(x, y),
                        px(LINE_HEIGHT),
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                if self
                    .editor
                    .update(cx, |editor, cx| editor.caret_shown(window, cx))
                    && let Some(cursor) = prepaint.cursor.take()
                {
                    window.paint_quad(cursor);
                }
            },
        );
        let _ = prepaint.gutter_width;
    }
}

impl Render for CodeEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::save))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .size_full()
            .overflow_hidden()
            .child(EditorElement {
                editor: cx.entity(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_rows_align_with_editor_lines_after_a_trailing_newline() {
        // The syntax crate drops the empty row a final `\n` opens; the
        // editor keeps it. Both shapes must resolve spans for real rows.
        for source in [
            "fn main() {}\nfn other() {}\n",
            "fn main() {}\nfn other() {}",
        ] {
            let doc = cypher_syntax::highlight(HighlightRequest {
                source,
                path: Some("main.rs"),
                fence_tag: None,
            })
            .unwrap();
            let editor_lines = line_starts(source).len();
            let aligned = doc.lines.len() == editor_lines
                || (doc.lines.len() + 1 == editor_lines && source.ends_with('\n'));
            assert!(aligned, "{source:?}: {} vs {editor_lines}", doc.lines.len());
            assert!(!doc.lines[1].is_empty());
        }
    }

    #[test]
    fn line_starts_follow_newlines() {
        assert_eq!(line_starts(""), vec![0]);
        assert_eq!(line_starts("a"), vec![0]);
        assert_eq!(line_starts("a\nb"), vec![0, 2]);
        assert_eq!(line_starts("a\nb\n"), vec![0, 2, 4]);
    }

    #[test]
    fn tabs_expand_to_stops_for_display_only() {
        let line = DisplayLine::new("\tx\ty");
        assert_eq!(line.text.as_ref(), "    x   y");
        assert_eq!(line.to_display(0), 0);
        assert_eq!(line.to_display(1), 4);
        assert_eq!(line.to_display(2), 5);
        assert_eq!(line.to_display(3), 8);
        assert_eq!(line.to_display(4), 9);
        // Inside an expanded tab the caret snaps to the tab itself.
        assert_eq!(line.to_raw(2), 0);
        assert_eq!(line.to_raw(4), 1);
        assert_eq!(line.to_raw(6), 2);
        assert_eq!(line.to_raw(9), 4);
        assert_eq!(display_columns("\tx\ty"), 9);
        assert_eq!(byte_for_column("\tx\ty", 4), 1);
        assert_eq!(byte_for_column("\tx\ty", 2), 0);
        assert_eq!(byte_for_column("\tx\ty", 99), 4);
    }

    #[test]
    fn crlf_lines_hide_the_carriage_return() {
        let line = DisplayLine::new("ab\r");
        assert_eq!(line.text.as_ref(), "ab");
        assert_eq!(line.to_display(3), 2);
        assert_eq!(line.to_display(2), 2);
        assert_eq!(line.to_raw(2), 2);
        assert_eq!(line.to_raw(1), 1);
        assert_eq!(display_columns("ab\r"), 2);
        let plain = DisplayLine::new("ab");
        assert!(plain.map.is_none());
        assert_eq!(plain.to_display(2), 2);
    }

    #[test]
    fn multibyte_lines_map_char_boundaries() {
        let line = DisplayLine::new("é\tz");
        assert_eq!(line.text.as_ref(), "é   z");
        assert_eq!(line.to_display(2), 2);
        assert_eq!(line.to_display(3), 5);
        assert_eq!(line.to_raw(5), 3);
        assert_eq!(line.to_raw(3), 2);
        let spans = line.shift_spans(&[HighlightSpan {
            range: 3..4,
            kind: cypher_syntax::HighlightKind::Keyword,
        }]);
        assert_eq!(spans[0].range, 5..6);
    }

    #[test]
    fn reveal_moves_the_least_and_clamps() {
        assert_eq!(scroll_to_reveal(0.0, 100.0, 20.0, 1000.0, 200.0), 0.0);
        assert_eq!(scroll_to_reveal(0.0, 300.0, 20.0, 1000.0, 200.0), 120.0);
        assert_eq!(scroll_to_reveal(500.0, 100.0, 20.0, 1000.0, 200.0), 100.0);
        assert_eq!(scroll_to_reveal(900.0, 990.0, 20.0, 1000.0, 200.0), 800.0);
        assert_eq!(scroll_to_reveal(50.0, 0.0, 20.0, 100.0, 200.0), 0.0);
    }

    #[test]
    fn auto_indent_copies_leading_whitespace_up_to_the_caret() {
        assert_eq!(auto_indent("    let x = 1;", 14), "    ");
        assert_eq!(auto_indent("    let x = 1;", 2), "  ");
        assert_eq!(auto_indent("\t\tfoo", 5), "\t\t");
        assert_eq!(auto_indent("foo", 3), "");
        assert_eq!(auto_indent("   ", 3), "   ");
    }
}
