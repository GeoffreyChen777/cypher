//! Justified text: a wrapped paragraph whose every row but a paragraph's
//! last fills the width, the slack shared out between its words. gpui's
//! text aligns left, centre or right only, so this element wraps with the
//! text system, then shapes each word of a full row on its own and places
//! it. Inline-code washes and links are drawn and hit-tested here too; text
//! selection, find, math and file chips are not (callers fall back to the
//! ordinary text element for those).

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, BorderStyle, Bounds, CursorStyle, DispatchPhase, Element, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseMoveEvent, MouseUpEvent, Pixels, ShapedLine, SharedString, Size, TextAlign, TextRun,
    Window, point, px, quad, size,
};

/// A row's slack per gap above which it stays ragged: stretching a short
/// row of two words across the width reads worse than a ragged edge.
const MAX_GAP_EM: f32 = 1.6;

/// What a link click is handed: its target.
pub type LinkClick = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Inline-code washes, as the markdown renderer paints them.
#[derive(Clone)]
pub struct CodeWash {
    pub ranges: Vec<Range<usize>>,
    pub color: Hsla,
    pub radius: f32,
    pub pad_x: f32,
    /// The wash's height as a multiple of the font size.
    pub height_em: f32,
}

/// One shaped piece of a row: a word (or a whole ragged row), where it
/// starts in the text and how far in from the left it sits.
struct Piece {
    range: Range<usize>,
    line: ShapedLine,
    x: Pixels,
}

struct Laid {
    width: Option<Pixels>,
    rows: Vec<Vec<Piece>>,
    line_height: Pixels,
    font_size: Pixels,
}

pub struct JustifiedText {
    text: SharedString,
    runs: Option<Vec<TextRun>>,
    code: Option<CodeWash>,
    links: Vec<(Range<usize>, String)>,
    on_link: Option<LinkClick>,
    laid: Rc<RefCell<Option<Laid>>>,
}

impl JustifiedText {
    /// `runs` cover `text` (`None`: the inherited text style throughout).
    pub fn new(text: impl Into<SharedString>, runs: Option<Vec<TextRun>>) -> Self {
        Self {
            text: text.into(),
            runs,
            code: None,
            links: Vec::new(),
            on_link: None,
            laid: Rc::default(),
        }
    }

    pub fn code(mut self, code: CodeWash) -> Self {
        self.code = Some(code);
        self
    }

    pub fn links(mut self, links: Vec<(Range<usize>, String)>, on_link: LinkClick) -> Self {
        self.links = links;
        self.on_link = Some(on_link);
        self
    }
}

/// The words of `row` (byte ranges into `text`): split at ordinary spaces,
/// except inside `keep` ranges (inline code stays one word), trimmed. A thin
/// space (the margin beside a code chip) joins, so punctuation after a chip
/// stays with it.
pub fn row_words(text: &str, row: Range<usize>, keep: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (offset, ch) in text[row.clone()].char_indices() {
        let ix = row.start + offset;
        let kept = keep.iter().any(|k| k.start <= ix && ix < k.end);
        if (ch == ' ' || ch == '\t') && !kept {
            if let Some(s) = start.take() {
                words.push(s..ix);
            }
        } else if start.is_none() {
            start = Some(ix);
        }
    }
    if let Some(s) = start {
        words.push(s..row.end);
    }
    words
}

/// `runs` cut to `range` of the text they cover.
fn runs_in(runs: &[TextRun], range: &Range<usize>) -> Vec<TextRun> {
    let mut out = Vec::new();
    let mut at = 0;
    for run in runs {
        let (start, end) = (at, at + run.len);
        at = end;
        let (s, e) = (start.max(range.start), end.min(range.end));
        if s < e {
            out.push(TextRun {
                len: e - s,
                ..run.clone()
            });
        }
    }
    out
}

/// Wrap `text` at `width` and place every row's pieces.
fn lay_out(
    text: &SharedString,
    runs: &[TextRun],
    keep: &[Range<usize>],
    font_size: Pixels,
    width: Option<Pixels>,
    window: &mut Window,
) -> Vec<Vec<Piece>> {
    let system = window.text_system().clone();
    let shape = |range: &Range<usize>| {
        system.shape_line(
            SharedString::from(text[range.clone()].to_string()),
            font_size,
            &runs_in(runs, range),
            None,
        )
    };
    let Ok(lines) = system.shape_text(text.clone(), font_size, runs, width, None) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut base = 0;
    for line in lines.iter() {
        let len = line.len();
        let layout = &line.unwrapped_layout;
        let mut starts: Vec<usize> = line
            .wrap_boundaries
            .iter()
            .filter_map(|b| {
                layout
                    .runs
                    .get(b.run_ix)
                    .and_then(|run| run.glyphs.get(b.glyph_ix))
                    .map(|glyph| glyph.index)
            })
            .collect();
        starts.insert(0, 0);
        starts.push(len);
        let count = starts.len() - 1;
        for (ix, pair) in starts.windows(2).enumerate() {
            let row = base + pair[0]..base + pair[1];
            let words = row_words(text, row.clone(), keep);
            let (Some(first), Some(last)) = (words.first(), words.last()) else {
                rows.push(Vec::new());
                continue;
            };
            let whole = first.start..last.end;
            let ragged = || {
                vec![Piece {
                    line: shape(&whole),
                    range: whole.clone(),
                    x: px(0.0),
                }]
            };
            // A paragraph's last row, or one with nothing to share out,
            // stays ragged.
            let last_row = ix + 1 == count;
            let pieces = match width {
                Some(width) if !last_row && words.len() > 1 => {
                    let shaped: Vec<ShapedLine> = words.iter().map(&shape).collect();
                    let used: f32 = shaped.iter().map(|l| f32::from(l.width())).sum();
                    let gap = (f32::from(width) - used) / (words.len() - 1) as f32;
                    if gap < 0.0 || gap > f32::from(font_size) * MAX_GAP_EM {
                        ragged()
                    } else {
                        let mut x = 0.0;
                        words
                            .iter()
                            .zip(shaped)
                            .map(|(range, line)| {
                                let piece = Piece {
                                    range: range.clone(),
                                    x: px(x),
                                    line,
                                };
                                x += f32::from(piece.line.width()) + gap;
                                piece
                            })
                            .collect()
                    }
                }
                _ => ragged(),
            };
            rows.push(pieces);
        }
        base += len + 1;
    }
    rows
}

/// The rectangles `range` covers, row by row, `pad_x` wider each side.
fn range_boxes(
    rows: &[Vec<Piece>],
    range: &Range<usize>,
    origin: gpui::Point<Pixels>,
    line_height: Pixels,
    pad_x: f32,
    inset_y: f32,
) -> Vec<Bounds<Pixels>> {
    let mut out = Vec::new();
    for (row_ix, row) in rows.iter().enumerate() {
        let y = origin.y + line_height * row_ix as f32;
        for piece in row {
            let (s, e) = (
                range.start.max(piece.range.start),
                range.end.min(piece.range.end),
            );
            if s >= e {
                continue;
            }
            let x0 = origin.x + piece.x + piece.line.x_for_index(s - piece.range.start);
            let x1 = origin.x + piece.x + piece.line.x_for_index(e - piece.range.start);
            out.push(Bounds::from_corners(
                point(x0 - px(pad_x), y + px(inset_y)),
                point(x1 + px(pad_x), y + line_height - px(inset_y)),
            ));
        }
    }
    out
}

impl Element for JustifiedText {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.pixel_snap(
            style
                .line_height
                .to_pixels(font_size.into(), window.rem_size()),
        );
        let runs = self
            .runs
            .clone()
            .unwrap_or_else(|| vec![style.to_run(self.text.len())]);
        let keep: Vec<Range<usize>> = self
            .code
            .as_ref()
            .map(|c| c.ranges.clone())
            .unwrap_or_default();
        let (text, laid) = (self.text.clone(), self.laid.clone());
        let id = window.request_measured_layout(
            Default::default(),
            move |known: Size<Option<Pixels>>, available: Size<AvailableSpace>, window, _| {
                let width = known.width.or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    _ => None,
                });
                let cached = laid
                    .borrow()
                    .as_ref()
                    .is_some_and(|l| l.width == width && l.font_size == font_size);
                if !cached {
                    let rows = lay_out(&text, &runs, &keep, font_size, width, window);
                    *laid.borrow_mut() = Some(Laid {
                        width,
                        rows,
                        line_height,
                        font_size,
                    });
                }
                let laid = laid.borrow();
                let laid = laid.as_ref().expect("laid out above");
                let natural = laid
                    .rows
                    .iter()
                    .filter_map(|row| row.last().map(|p| p.x + p.line.width()))
                    .fold(px(0.0), |a, b| a.max(b));
                size(
                    width.unwrap_or(natural),
                    line_height * laid.rows.len().max(1) as f32,
                )
            },
        );
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<Hitbox> {
        (!self.links.is_empty()).then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let laid = self.laid.borrow();
        let Some(laid) = laid.as_ref() else {
            return;
        };
        let origin = bounds.origin;
        let line_height = laid.line_height;
        if let Some(code) = &self.code {
            let wash_h = f32::from(laid.font_size) * code.height_em;
            let inset_y = ((f32::from(line_height) - wash_h) / 2.0).max(0.0);
            for range in &code.ranges {
                for rect in range_boxes(&laid.rows, range, origin, line_height, code.pad_x, inset_y)
                {
                    window.paint_quad(quad(
                        rect,
                        px(code.radius),
                        code.color,
                        px(0.0),
                        gpui::transparent_black(),
                        BorderStyle::default(),
                    ));
                }
            }
        }
        for (row_ix, row) in laid.rows.iter().enumerate() {
            let y = origin.y + line_height * row_ix as f32;
            for piece in row {
                let _ = piece.line.paint(
                    point(origin.x + piece.x, y),
                    line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }
        // Links: a pointer over them, and a click opens one.
        let (Some(hitbox), Some(on_link)) = (hitbox.as_ref(), self.on_link.clone()) else {
            return;
        };
        let targets: Vec<(Bounds<Pixels>, String)> = self
            .links
            .iter()
            .flat_map(|(range, url)| {
                range_boxes(&laid.rows, range, origin, line_height, 0.0, 0.0)
                    .into_iter()
                    .map(move |rect| (rect, url.clone()))
            })
            .collect();
        let over = |at: gpui::Point<Pixels>| targets.iter().find(|(r, _)| r.contains(&at));
        if hitbox.is_hovered(window) && over(window.mouse_position()).is_some() {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        let was_over = over(window.mouse_position()).is_some();
        let moved = targets.iter().map(|(r, _)| *r).collect::<Vec<_>>();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble
                && moved.iter().any(|r| r.contains(&event.position)) != was_over
            {
                window.refresh();
            }
        });
        let hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && hitbox.is_hovered(window)
                && let Some((_, url)) = targets.iter().find(|(r, _)| r.contains(&event.position))
            {
                on_link(url, window, cx);
            }
        });
    }
}

impl IntoElement for JustifiedText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::row_words;

    fn words<'a>(text: &'a str, keep: &[std::ops::Range<usize>]) -> Vec<&'a str> {
        row_words(text, 0..text.len(), keep)
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[test]
    fn rows_split_at_spaces_and_trim() {
        assert_eq!(words("  run the  suite ", &[]), ["run", "the", "suite"]);
        assert_eq!(words("", &[]), Vec::<&str>::new());
    }

    #[test]
    fn inline_code_stays_one_word() {
        let text = "add currency: str to Invoice";
        let code = 4..17;
        assert_eq!(&text[code.clone()], "currency: str");
        assert_eq!(
            words(text, &[code]),
            ["add", "currency: str", "to", "Invoice"]
        );
    }

    #[test]
    fn a_thin_space_beside_code_does_not_split() {
        assert_eq!(
            words("\u{2009}test_tax.py\u{2009}: 2 flaky", &[]),
            ["\u{2009}test_tax.py\u{2009}:", "2", "flaky"]
        );
    }

    #[test]
    fn a_row_inside_the_text_keeps_its_offsets() {
        let text = "one two three four";
        let ranges = super::row_words(text, 4..13, &[]);
        assert_eq!(ranges, [4..7, 8..13]);
    }
}
