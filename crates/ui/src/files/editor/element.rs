//! The editor's custom element: lays out the visible lines, then paints
//! the gutter, selection, text and caret.

use super::*;

// ---------------------------------------------------------------------------
// Element
// ---------------------------------------------------------------------------

pub(super) struct EditorElement {
    pub(super) editor: Entity<CodeEditor>,
}

pub(super) struct EditorPrepaint {
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
            crate::kit::theme::wash(0.035),
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
