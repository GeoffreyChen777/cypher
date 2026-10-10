//! The custom element: measured auto-grow layout + shaped-line painting.

use std::rc::Rc;

use gpui::{
    AnyTooltip, App, BorderStyle, Bounds, DispatchPhase, ElementInputHandler, GlobalElementId,
    IntoElement, LayoutId, MouseMoveEvent, PaintQuad, Pixels, Point, SharedString, Style, TextRun,
    Window, fill, point, px, quad, relative, size,
};

use super::{CHIP_TOOLTIP_HEIGHT, ChipHit, ChipTooltipTarget, TextInputElement, TextInputEvent};

pub(super) struct TextInputPrepaint {
    cursor: Option<PaintQuad>,
    chip_quads: Vec<PaintQuad>,
    chip_hits: Vec<ChipHit>,
    selection_quads: Vec<PaintQuad>,
    /// Completion preview: window-space origin of the end-of-text caret plus
    /// the suffix to paint there (shaped at paint time — it never joins the
    /// content's own layout, so hit-testing and the caret ignore it).
    ghost: Option<(Point<Pixels>, SharedString)>,
}

impl IntoElement for TextInputElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for TextInputElement {
    type RequestLayoutState = ();
    type PrepaintState = TextInputPrepaint;

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
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let input = self.input.clone();
        let text_style = window.text_style();
        let max_content = self.max_content_height;
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, cx| {
                let width = known.width.unwrap_or(match available.width {
                    gpui::AvailableSpace::Definite(width) => width,
                    _ => px(320.0),
                });
                let content_height = input.update(cx, |input, cx| {
                    input.layout_text(width, &text_style, window, cx)
                });
                size(width, px(content_height.min(max_content)))
            });
        (layout_id, ())
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
        self.input.update(cx, |input, cx| {
            let scrolled = input.clamp_scroll(f32::from(bounds.size.height));
            input.last_bounds = Some(bounds);
            if scrolled {
                cx.emit(TextInputEvent::ViewportChanged);
            }
        });
        let input = self.input.read(cx);
        let scroll = px(input.scroll_top);
        let origin = point(bounds.left(), bounds.top() - scroll);
        let input_theme = input.text_theme(cx);
        let selection_color = input_theme.selection;
        let caret_color = input_theme.caret;
        // The inline-code recipe: chips wash emerald like `code` spans do.
        let chip_color = input_theme.code_wash;

        let mut chip_quads = Vec::new();
        let mut chip_hits = Vec::new();
        for (chip, display) in &input.projection.chips {
            let target = ChipTooltipTarget {
                range: chip.range.clone(),
                label: chip.tooltip.clone(),
            };
            for local_bounds in input.bounds_for_display_range(display.clone()) {
                let chip_bounds = Bounds::new(
                    point(
                        origin.x + local_bounds.origin.x,
                        origin.y + local_bounds.origin.y + px(2.0),
                    ),
                    size(local_bounds.size.width, local_bounds.size.height - px(4.0)),
                );
                chip_quads.push(quad(
                    chip_bounds,
                    px(5.0),
                    chip_color,
                    px(0.0),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
                let above_anchor = chip_bounds.top() - px(CHIP_TOOLTIP_HEIGHT) - px(1.0);
                let anchor_y = if above_anchor >= px(0.0) {
                    above_anchor
                } else {
                    // GPUI positions at anchor + 1px; subtracting one keeps the
                    // below fallback flush so the pointer can enter the popup.
                    chip_bounds.bottom() - px(1.0)
                };
                let visible_bounds = chip_bounds.intersect(&bounds);
                if visible_bounds.size.width == px(0.0) || visible_bounds.size.height == px(0.0) {
                    continue;
                }
                chip_hits.push(ChipHit {
                    target: target.clone(),
                    bounds: visible_bounds,
                    // The fixed-height popup starts at anchor + 1px. Moving
                    // the anchor above the chip therefore yields conventional
                    // above-target placement without cursor tracking.
                    anchor: point(chip_bounds.left(), anchor_y),
                });
            }
        }
        let mut selection_quads = Vec::new();
        let mut cursor = None;
        if input.selected_range.is_empty() || input.display_is_placeholder {
            if let Some(p) = input.point_for_index(input.cursor_offset()) {
                cursor = Some(fill(
                    Bounds::new(
                        point(origin.x + p.x, origin.y + p.y),
                        size(px(2.0), input.line_height),
                    ),
                    caret_color,
                ));
            } else if input.display_is_placeholder {
                cursor = Some(fill(
                    Bounds::new(origin, size(px(2.0), input.line_height)),
                    caret_color,
                ));
            }
        } else if let (Some(start), Some(end)) = (
            input.point_for_index(input.selected_range.start),
            input.point_for_index(input.selected_range.end),
        ) {
            let lh = input.line_height;
            if start.y == end.y {
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(origin.x + end.x, origin.y + start.y + lh),
                    ),
                    selection_color,
                ));
            } else {
                // First visual row, full middle rows, last visual row.
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(bounds.right(), origin.y + start.y + lh),
                    ),
                    selection_color,
                ));
                if end.y > start.y + lh {
                    selection_quads.push(fill(
                        Bounds::from_corners(
                            point(origin.x, origin.y + start.y + lh),
                            point(bounds.right(), origin.y + end.y),
                        ),
                        selection_color,
                    ));
                }
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x, origin.y + end.y),
                        point(origin.x + end.x, origin.y + end.y + lh),
                    ),
                    selection_color,
                ));
            }
        }
        let tooltip = input.visible_chip_tooltip();
        if let Some((_target, anchor, _activation, view)) = tooltip {
            let view = view.into();
            let input = self.input.clone();
            window.set_tooltip(AnyTooltip {
                view,
                mouse_position: anchor,
                check_visible_and_update: Rc::new(move |popup, window, cx| {
                    input.update(cx, |input, _| {
                        input.check_chip_tooltip_visibility(popup, window.mouse_position())
                    })
                }),
            });
        }
        // The ghost only shows where accepting it would insert: a collapsed
        // caret at the end of real (non-placeholder, non-IME) text.
        let ghost = input
            .ghost
            .clone()
            .filter(|g| {
                !g.is_empty()
                    && !input.display_is_placeholder
                    && input.marked_range.is_none()
                    && input.selected_range.is_empty()
                    && input.cursor_offset() == input.content.len()
            })
            .and_then(|g| {
                input
                    .point_for_index(input.content.len())
                    .map(|p| (point(origin.x + p.x, origin.y + p.y), g))
            });
        TextInputPrepaint {
            cursor,
            chip_quads,
            chip_hits,
            selection_quads,
            ghost,
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
        let focus_handle = self.input.read(cx).focus_handle.clone();
        self.input.update(cx, |input, _| {
            input.set_chip_hits(prepaint.chip_hits.clone())
        });
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |input, cx| input.on_mouse_move(event, cx));
            }
        });

        // WrappedLine isn't Clone — temporarily take the shaped lines out of the
        // entity for painting, then put them back for mouse mapping.
        let (lines, line_height, scroll) = self.input.update(cx, |input, _| {
            (
                std::mem::take(&mut input.last_lines),
                input.line_height,
                input.scroll_top,
            )
        });

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.chip_quads.drain(..) {
                window.paint_quad(quad);
            }
            for quad in prepaint.selection_quads.drain(..) {
                window.paint_quad(quad);
            }
            let mut y = bounds.top() - px(scroll);
            for line in &lines {
                let height = line.size(line_height).height;
                let _ = line.paint(
                    point(bounds.left(), y),
                    line_height,
                    gpui::TextAlign::Left,
                    Some(bounds),
                    window,
                    cx,
                );
                y += height;
            }
            if let Some((ghost_origin, ghost)) = prepaint.ghost.take() {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let run = TextRun {
                    len: ghost.len(),
                    font: style.font(),
                    color: self.input.read(cx).text_theme(cx).text_faint,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window
                    .text_system()
                    .shape_line(ghost, font_size, &[run], None);
                // (Clipping comes from the surrounding content mask.)
                let _ = line.paint(
                    ghost_origin,
                    line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            // Caret only when this input is actually focused in an active
            // window (Electron hides it on window deactivation too), and only
            // in the "on" blink phase — solid while typing, ~500ms blink idle.
            if self
                .input
                .update(cx, |input, cx| input.caret_shown(window, cx))
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        self.input.update(cx, |input, _| {
            input.last_lines = lines;
        });
    }
}
