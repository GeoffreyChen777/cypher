//! Caret movement, selection and editing commands — the bodies behind the
//! editor's key bindings.

use super::*;

impl CodeEditor {
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

    pub(super) fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp_boundary(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.last_edit = None;
        self.reset_blink();
        cx.notify();
    }

    pub(super) fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
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

    pub(super) fn next_boundary(&self, offset: usize) -> usize {
        self.content[offset..]
            .graphemes(true)
            .next()
            .map(|g| offset + g.len())
            .unwrap_or(self.content.len())
    }

    pub(super) fn previous_word_boundary(&self, offset: usize) -> usize {
        self.content[..offset]
            .split_word_bound_indices()
            .rev()
            .find_map(|(ix, word)| (!word.trim().is_empty()).then_some(ix))
            .unwrap_or(0)
    }

    pub(super) fn next_word_boundary(&self, offset: usize) -> usize {
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

    pub(super) fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let start = self.previous_boundary(self.cursor_offset());
            self.selected_range = start..self.cursor_offset();
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    pub(super) fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let end = self.next_boundary(self.cursor_offset());
            self.selected_range = self.cursor_offset()..end;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    pub(super) fn delete_word_left(
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

    pub(super) fn delete_word_right(
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

    pub(super) fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    pub(super) fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    pub(super) fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(-1), cx);
    }

    pub(super) fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(1), cx);
    }

    pub(super) fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_target(-1), cx);
    }

    pub(super) fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_target(1), cx);
    }

    pub(super) fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(-self.page_lines()), cx);
    }

    pub(super) fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical_target(self.page_lines()), cx);
    }

    pub(super) fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    pub(super) fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    pub(super) fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word_boundary(self.cursor_offset()), cx);
    }

    pub(super) fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word_boundary(self.cursor_offset()), cx);
    }

    pub(super) fn select_word_left(
        &mut self,
        _: &SelectWordLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.previous_word_boundary(self.cursor_offset()), cx);
    }

    pub(super) fn select_word_right(
        &mut self,
        _: &SelectWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.next_word_boundary(self.cursor_offset()), cx);
    }

    pub(super) fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    pub(super) fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.move_to(line.start, cx);
    }

    pub(super) fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.move_to(line.end, cx);
    }

    pub(super) fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.select_to(line.start, cx);
    }

    pub(super) fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range(self.line_of(self.cursor_offset()));
        self.select_to(line.end, cx);
    }

    pub(super) fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    pub(super) fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    pub(super) fn select_doc_start(
        &mut self,
        _: &SelectDocStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }

    pub(super) fn select_doc_end(
        &mut self,
        _: &SelectDocEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.content.len(), cx);
    }

    pub(super) fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    pub(super) fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    pub(super) fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    pub(super) fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.selected_range.start;
        let line = self.line_range(self.line_of(cursor));
        let indent = auto_indent(&self.content[line.clone()], cursor - line.start).to_string();
        self.replace_text_in_range(None, &format!("\n{indent}"), window, cx);
    }

    /// Tab inserts a real tab on tab-indented lines, else spaces up to the
    /// next stop — never shifts selections (kept deliberately small).
    pub(super) fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(CodeEditorEvent::Save);
    }
}
