//! The platform text-input bridge: UTF-16 ranges, IME marked text and
//! character geometry for the editor.

use super::*;

/// UTF-16 offset → byte offset, measured *within `text`*.
///
/// The two are interchangeable only while the text stays in the BMP's
/// single-byte range; every CJK character widens the byte offset by two past
/// the UTF-16 one, so the string the offset was expressed against is the one it
/// has to be resolved against.
pub(super) fn utf16_to_byte_offset(text: &str, offset: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;
    for ch in text.chars() {
        if utf16_count >= offset {
            break;
        }
        utf16_count += ch.len_utf16();
        utf8_offset += ch.len_utf8();
    }
    utf8_offset
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
        // `new_selected_range_utf16` is scoped to `new_text` (it comes straight
        // from `setMarkedText:selectedRange:`), so it has to be measured inside
        // `new_text` and only then rebased onto the document. Measuring it
        // against the whole buffer drifts the caret by however much wider the
        // preceding text is in UTF-8 than in UTF-16 — which is exactly what a
        // line mixing CJK with Latin does.
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| {
                range.start + utf16_to_byte_offset(new_text, r.start)
                    ..range.start + utf16_to_byte_offset(new_text, r.end)
            })
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
