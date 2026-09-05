//! BlockTree → gpui elements.
//!
//! Numbers drive layout (font sizes, line heights, paddings — all constants
//! here); colors are paint. Code blocks render per-line so their height is
//! exactly `lines × line_height`, and syntax highlighting arrives later as
//! recolored `TextRun`s on the identical mono font — layout never changes
//! (mugen's "highlight is pure paint"). Streaming fade-in is a per-appended-
//! chunk opacity veil over the text runs (see [`super::veil`]) — opacity only,
//! zero translate, applied after layout-relevant properties are fixed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use cypher_syntax::{HighlightKind, HighlightSpan, HighlightedDocument};
use gpui::{
    AnyElement, BorderStyle, Bounds, FontStyle, FontWeight, Hsla, InteractiveText, SharedString,
    StyledText, TextRun, UnderlineStyle, Window, canvas, div, font, point, prelude::*, px, quad,
    size,
};

use crate::theme::Theme;

use super::parser::{Block, BlockTree, InlineRun, TableAlign};
use super::veil::{RowVeil, apply_veil, slice_spans};

/// Gap between markdown blocks inside one message (zeron mdBlockGap).
pub const MD_BLOCK_GAP: f32 = 12.0;
/// Body text size / line height (zeron: 14px / 22px).
pub const MD_TEXT_SIZE: f32 = 14.0;
pub const MD_LINE_HEIGHT: f32 = 22.0;
/// Code block metrics — height is `lines × CODE_LINE_HEIGHT + padding + header`.
pub const CODE_TEXT_SIZE: f32 = 12.5;
pub const CODE_LINE_HEIGHT: f32 = 18.0;
pub const CODE_PADDING_X: f32 = 12.0;
pub const CODE_PADDING_Y: f32 = 10.0;

// Table metrics — a port of mugen-markdown 0.6.2's `TableBlock` under zeron's
// resolved md theme. The design is frameless ("flat hairline"): 1px horizontal
// rules under the header and between rows are the only chrome — no outer box,
// no header fill, no corner radius (theme: headerBackground transparent,
// radius 0). Cells use the body scale (14/22) with a uniform 12px padding;
// the header row is weight-700 per `table.headerWeight`.
/// Uniform cell padding in px (zeron `table.cellPadding`).
pub const TABLE_CELL_PADDING: f32 = 12.0;
/// Hairline between rows in px (zeron `table.gap`).
pub const TABLE_DIVIDER: f32 = 1.0;
/// Header row font weight (zeron `table.headerWeight` = 700).
pub const TABLE_HEADER_WEIGHT: FontWeight = FontWeight::BOLD;
/// Floor for a column's max-content share, so a short column ("1k") beside a
/// prose column keeps a readable width (mugen `MIN_COLUMN_CONTENT`).
pub const TABLE_MIN_COLUMN_CONTENT: f32 = 48.0;
/// Minimum rendered column width in px, padding included (zeron
/// `table.minColumnWidth`). Naturally narrower columns keep their content
/// width; wider ones wrap down to this floor, then the table scrolls.
pub const TABLE_MIN_COLUMN_WIDTH: f32 = 96.0;
/// Hairline tone (zeron md theme `table.borderColor`: rgba(255,255,255,0.1)).
pub fn table_hairline() -> Hsla {
    crate::theme::hairline(0.10)
}

/// Options for one rendered tree (a transcript row or a whole live message).
pub struct RenderOptions {
    /// Stable row key — prefixes element ids (scroll state, animations).
    pub row_key: SharedString,
    /// Streaming veil state for a live row: newly appended text fades in via
    /// paint-only run opacity, keyed per (element, chunk offset) so each chunk
    /// fades exactly once. `None` renders without fades (completed rows).
    pub veil: Option<Rc<RefCell<RowVeil>>>,
    /// Flatten/shape input cache (see [`RenderCache`]): settled blocks reuse
    /// their flat text + runs across frames instead of rebuilding them — the
    /// per-frame cost of a fading live row stays O(tail block), flat in the
    /// total reply length. `None` rebuilds every pass.
    pub cache: Option<Rc<RefCell<RenderCache>>>,
    /// Frame timestamp driving veil opacities (one clock per render pass).
    pub now: Instant,
    /// Code-block copy-button plumbing (round 9): `None` renders no button
    /// (previews outside the transcript).
    pub copy: Option<CopyUi>,
    /// Text-selection lifecycle callbacks (round 19): the surface wires
    /// these to show/clear the Comment pill. `None` renders inert (previews).
    pub selection: Option<SelectionUi>,
    /// Which surface scope this tree belongs to — scopes the selection
    /// registry/wash so the transcript and each diff pane select
    /// independently (paint order cannot conflict). Only read when
    /// [`Self::selection`] is `Some`.
    pub scope: super::selection::SelectionScope,
}

/// Selection lifecycle callbacks fired from the frame-scoped selection mouse
/// listeners ([`register_selection_listeners`]) so the owning surface can
/// show and clear the Comment pill. Cloneable `Rc` closures — one instance
/// rides every `RenderOptions` of a transcript.
#[derive(Clone)]
pub struct SelectionUi {
    /// A new drag / double / triple-click selection began (mouse-down).
    pub on_started: Rc<dyn Fn(&mut Window, &mut gpui::App)>,
    /// The settled selection was cleared by a mouse-down outside it.
    pub on_cleared: Rc<dyn Fn(&mut Window, &mut gpui::App)>,
    /// A non-empty selection settled on mouse-up, with the mouse-up position
    /// in WINDOW coordinates (the pill's anchor).
    pub on_settled: Rc<
        dyn Fn(
            super::selection::SelectionSnapshot,
            gpui::Point<gpui::Pixels>,
            &mut Window,
            &mut gpui::App,
        ),
    >,
}

/// Copy-button wiring for one row's code blocks: the handler writes the code
/// to the clipboard and flips a transient per-row "Copied" state owned by the
/// transcript entity; `copied_ix` is the block currently showing feedback.
#[derive(Clone)]
pub struct CopyUi {
    pub handler: Rc<dyn Fn(usize, SharedString, &mut Window, &mut gpui::App)>,
    pub copied_ix: Option<usize>,
}

impl RenderOptions {
    /// Options for a completed (non-streaming) row — no veil, no cache.
    pub fn settled(row_key: SharedString) -> Self {
        Self {
            row_key,
            veil: None,
            cache: None,
            now: Instant::now(),
            copy: None,
            selection: None,
            scope: super::selection::SelectionScope::Transcript,
        }
    }
}

/// Cross-frame cache of flatten results, keyed by
/// `(row key, top-level block ix, element discriminator)`.
///
/// During a streaming fade the live row re-renders every frame; without the
/// cache each frame re-derives every block's flat `String` + `TextRun`s —
/// O(reply length) per frame, growing through long replies. The incremental
/// parser only ever touches a suffix of the top-level blocks
/// ([`super::parser::IncrementalParser::stable_prefix_blocks`]), so everything
/// below that boundary is byte-identical and its flatten result (and, via
/// gpui's line-layout cache keyed on identical text+runs, its shaping) can be
/// reused as-is. `SharedString`/`Rc` make the reuse O(1) per block.
/// Cached runs carry a resolved [`gpui::Hsla`] per span, so an entry is only
/// valid for the palette that produced it — content-only keys silently serve
/// dark-mode text onto a light background after an appearance switch.
/// [`RenderCache::sync_palette`] drops everything when the palette moves.
#[derive(Default)]
pub struct RenderCache {
    flats: HashMap<(SharedString, usize, usize), Rc<FlatText>>,
    code: HashMap<(SharedString, usize, usize), Rc<CachedCode>>,
    /// The [`crate::theme::theme_generation`] these entries were shaped under.
    generation: u32,
    style_revision: u64,
}

/// Cached per-line code runs (validity: code length + highlight identity).
pub struct CachedCode {
    code_len: usize,
    /// Slice-pointer identity + len of the highlight Arc that produced this.
    hl_key: (usize, usize),
    lines: Vec<(SharedString, Vec<TextRun>)>,
}

impl RenderCache {
    /// Drop every cached entry for `row`.
    pub fn invalidate_row(&mut self, row: &str) {
        self.flats.retain(|(r, _, _), _| r.as_ref() != row);
        self.code.retain(|(r, _, _), _| r.as_ref() != row);
    }

    pub fn clear(&mut self) {
        self.flats.clear();
        self.code.clear();
    }

    /// Drop every entry if the palette changed since they were shaped. Cheap
    /// enough (one relaxed atomic load) to call on every cache access.
    fn sync_palette(&mut self, theme: &Theme) {
        let generation = crate::theme::theme_generation();
        if self.generation != generation || self.style_revision != theme.text_style_revision {
            self.clear();
            self.generation = generation;
            self.style_revision = theme.text_style_revision;
        }
    }
}

/// Per-line highlight tokens for a code block, or `None` while pending.
pub type CodeHighlight<'a> = Option<&'a [Vec<HighlightSpan>]>;

/// Render a whole tree stacked with the md block gap. `highlight` resolves
/// tokens for a top-level block index (code blocks only).
pub fn render_tree(
    tree: &BlockTree,
    opts: &RenderOptions,
    theme: &Theme,
    window: &Window,
    highlight: &dyn Fn(usize) -> Option<std::sync::Arc<HighlightedDocument>>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(theme.markdown.block_gap))
        .children(tree.blocks.iter().enumerate().map(|(ix, top)| {
            let document = highlight(ix);
            render_block(
                &top.block,
                ix,
                ix,
                opts,
                theme,
                window,
                document
                    .as_deref()
                    .map(|document| document.lines.as_slice()),
            )
        }))
        .into_any_element()
}

/// Render one block (top-level or nested). `top_ix` is the enclosing top-level
/// block index (cache invalidation scope); `ix` the per-element discriminator.
#[allow(clippy::too_many_arguments)]
pub fn render_block(
    block: &Block,
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
    window: &Window,
    highlight: CodeHighlight,
) -> AnyElement {
    match block {
        Block::Paragraph { runs } => text_element(
            runs,
            theme.markdown.body_size,
            theme.markdown.body_line_height,
            false,
            top_ix,
            ix,
            opts,
            theme,
        ),
        Block::Heading { level, runs } => {
            let (size, line) = heading_metrics(*level);
            let size = size * theme.markdown.body_size / MD_TEXT_SIZE;
            let line = line * theme.markdown.body_line_height / MD_LINE_HEIGHT;
            text_element(runs, size, line, true, top_ix, ix, opts, theme)
        }
        Block::CodeBlock { language, code } => render_code_block(
            language.as_deref(),
            code,
            top_ix,
            ix,
            opts,
            theme,
            highlight,
        ),
        Block::BlockQuote { children } => div()
            // Accent-tinted quote: indigo rail + a whisper of the same hue
            // behind it (the inline-code treatment, dialed down).
            .border_l_2()
            .border_color(theme.accent.opacity(0.6))
            .bg(theme.accent.opacity(0.05))
            .rounded_tr(px(6.0))
            .rounded_br(px(6.0))
            .pl(px(12.0))
            .pr(px(10.0))
            .py(px(6.0))
            .flex()
            .flex_col()
            .gap(px(theme.markdown.block_gap * (8.0 / MD_BLOCK_GAP)))
            .text_color(theme.text_muted)
            .children(children.iter().enumerate().map(|(ci, child)| {
                render_block(child, top_ix, ix * 100 + ci, opts, theme, window, None)
            }))
            .into_any_element(),
        Block::List {
            ordered_start,
            items,
        } => div()
            .flex()
            .flex_col()
            .gap(px(theme.markdown.block_gap * (4.0 / MD_BLOCK_GAP)))
            .children(items.iter().enumerate().map(|(item_ix, item)| {
                // Accent markers (the inline-code hue): ordered numbers as
                // tinted text, unordered as a REAL 5px disc — the glyph "•"
                // reads too small at 14px.
                let marker: gpui::AnyElement = match ordered_start {
                    Some(start) => div()
                        .flex_none()
                        .min_w(px(18.0))
                        .text_size(px(theme.markdown.body_size))
                        .line_height(px(theme.markdown.body_line_height))
                        .text_color(theme.accent.opacity(0.85))
                        .child(SharedString::from(format!("{}.", start + item_ix as u64)))
                        .into_any_element(),
                    None => div()
                        .flex_none()
                        .min_w(px(18.0))
                        // Center the disc on the first text line's cap band.
                        .h(px(theme.markdown.body_line_height))
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .ml(px(1.0))
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(theme.accent.opacity(0.85)),
                        )
                        .into_any_element(),
                };
                div().flex().flex_row().gap(px(8.0)).child(marker).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(theme.markdown.block_gap * (4.0 / MD_BLOCK_GAP)))
                        .children(item.iter().enumerate().map(|(ci, child)| {
                            render_block(
                                child,
                                top_ix,
                                ix * 100 + item_ix * 10 + ci,
                                opts,
                                theme,
                                window,
                                None,
                            )
                        })),
                )
            }))
            .into_any_element(),
        Block::Table {
            header,
            rows,
            align,
        } => render_table(header, rows, align, top_ix, ix, opts, theme, window),
        Block::Rule => div()
            .h(px(1.0))
            .w_full()
            .bg(theme.border)
            .into_any_element(),
    }
}

/// Tight monochrome heading scale (zeron: h2 ≈ 16px semibold; headings step
/// down quickly toward body size).
fn heading_metrics(level: u8) -> (f32, f32) {
    match level {
        1 => (19.0, 27.0),
        2 => (16.0, 24.0),
        3 => (15.0, 22.0),
        _ => (14.0, 22.0),
    }
}

/// Shared per-column table geometry (port of mugen `tableColumns`).
pub struct TableColumns {
    /// Per-column max-content width, padding included.
    pub naturals: Vec<f32>,
    /// Per-column minimum width, padding included = `min(natural, minColumnWidth)`.
    pub minimums: Vec<f32>,
    /// Σ minimums — the width below which the table stops shrinking and scrolls.
    pub min_table_width: f32,
}

/// Resolve column geometry from measured per-column max-content widths
/// (content only — padding is added here, as the source adds `2 * cellPadding`).
pub fn table_columns(content_widths: &[f32]) -> TableColumns {
    let naturals: Vec<f32> = content_widths
        .iter()
        .map(|w| w.max(TABLE_MIN_COLUMN_CONTENT) + 2.0 * TABLE_CELL_PADDING)
        .collect();
    let minimums: Vec<f32> = naturals
        .iter()
        .map(|n| n.min(TABLE_MIN_COLUMN_WIDTH))
        .collect();
    let min_table_width = minimums.iter().sum();
    TableColumns {
        naturals,
        minimums,
        min_table_width,
    }
}

/// Element/cache discriminator for a table cell (row-major under the block ix).
fn table_cell_ix(ix: usize, r: usize, c: usize) -> usize {
    ix * 100_000 + r * 100 + c
}

/// A GFM table — a port of mugen-markdown's `TableBlock` under zeron's md
/// theme (see the `TABLE_*` constants).
///
/// Column widths resolve exactly the way the source's CSS does: each cell is
/// `flex: <max-content> <max-content> 0; min-width: min(max-content, 96px)`,
/// so widths are content-proportional with a readable per-column floor.
/// Naturals come from shaping each cell's runs unwrapped (gpui's line-layout
/// cache makes repeat frames cheap); the flex resolution itself is Taffy's —
/// the same algorithm as the web's. When even the floors no longer fit, the
/// rows overflow the viewport and the table scrolls horizontally instead of
/// crushing every column into per-character wrapping.
#[allow(clippy::too_many_arguments)]
fn render_table(
    header: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    align: &[TableAlign],
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
    window: &Window,
) -> AnyElement {
    // Header row first, mirroring the source's `rows` shape (rows may be ragged).
    let all: Vec<&[Vec<InlineRun>]> = std::iter::once(header)
        .filter(|h| !h.is_empty())
        .map(|h| h as &[Vec<InlineRun>])
        .chain(rows.iter().map(|r| r.as_slice()))
        .collect();
    let cols = all.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return gpui::Empty.into_any_element();
    }
    let has_header = !header.is_empty();

    // Flatten every cell (cache-aware) and take per-column max-content widths.
    let text_system = window.text_system();
    let mut flats: Vec<Vec<Option<Rc<FlatText>>>> = Vec::with_capacity(all.len());
    let mut content = vec![0.0f32; cols];
    for (r, row) in all.iter().enumerate() {
        let weight = if has_header && r == 0 {
            TABLE_HEADER_WEIGHT
        } else {
            FontWeight::NORMAL
        };
        let mut out: Vec<Option<Rc<FlatText>>> = Vec::with_capacity(cols);
        for (c, natural) in content.iter_mut().enumerate() {
            let Some(runs) = row.get(c) else {
                out.push(None);
                continue;
            };
            let flat = flatten_cached(runs, weight, top_ix, table_cell_ix(ix, r, c), opts, theme);
            if !flat.text.is_empty() {
                // Cell sources are single-line; guard anyway (same byte count,
                // so the runs still cover the text exactly).
                let line: SharedString = if flat.text.contains('\n') {
                    flat.text.replace('\n', " ").into()
                } else {
                    flat.text.clone()
                };
                let width = f32::from(
                    text_system
                        .shape_line(line, px(theme.markdown.body_size), &flat.runs, None)
                        .width(),
                );
                if width > *natural {
                    *natural = width;
                }
            }
            out.push(Some(flat));
        }
        flats.push(out);
    }
    let geo = table_columns(&content);

    // Frameless flat-hairline chrome: 1px rules under the header and between
    // rows are the only paint (`table.gap` = 1, borderColor white@10%); the
    // theme's headerBackground is transparent and its radius 0, so there is no
    // header fill, outer box, or rounding.
    let hairline = theme.hairline(0.10);
    let mut inner = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(geo.min_table_width));
    for (r, row) in flats.iter().enumerate() {
        if r > 0 {
            inner = inner.child(div().flex_none().h(px(TABLE_DIVIDER)).w_full().bg(hairline));
        }
        let mut row_el = div().flex().flex_row();
        for (c, cell_flat) in row.iter().enumerate() {
            let mut cell = div()
                .flex_grow(geo.naturals[c])
                .flex_shrink(geo.naturals[c])
                .flex_basis(px(0.0))
                .min_w(px(geo.minimums[c]))
                .p(px(TABLE_CELL_PADDING))
                .text_size(px(theme.markdown.body_size))
                .line_height(px(theme.markdown.body_line_height));
            cell = match align.get(c).copied().unwrap_or_default() {
                TableAlign::Left => cell,
                TableAlign::Center => cell.text_center(),
                TableAlign::Right => cell.text_right(),
            };
            if let Some(flat) = cell_flat {
                cell = cell.child(flat_text_element(
                    flat,
                    table_cell_ix(ix, r, c),
                    opts,
                    theme,
                ));
            }
            row_el = row_el.child(cell);
        }
        inner = inner.child(row_el);
    }

    // The horizontal scroller — when the floors exceed the viewport the inner
    // block keeps `min_table_width` and this viewport scrolls it.
    let scroll_id: SharedString = format!("{}-table{ix}", opts.row_key).into();
    div()
        .id(scroll_id)
        .w_full()
        .overflow_x_scroll()
        .child(inner)
        .into_any_element()
}

/// Flattened inline runs: one string + gpui `TextRun`s + clickable link ranges
/// + inline-code ranges (their rounded washes are painted by a canvas UNDER
/// the text — `TextRun::background_color` can only paint square boxes).
/// `text` is a `SharedString` so cached reuse across frames is an Arc clone.
pub struct FlatText {
    pub text: SharedString,
    pub runs: Vec<TextRun>,
    pub links: Vec<(Range<usize>, String)>,
    pub code_ranges: Vec<Range<usize>>,
}

/// Scoped inline-code tint. Defaults to emerald text and a translucent wash;
/// overrides do not change the shared tokens used by mention chips.
pub fn inline_code_text(theme: &Theme) -> Hsla {
    theme.inline_code_text.unwrap_or(theme.code_text)
}
pub fn inline_code_wash(theme: &Theme) -> Hsla {
    theme.inline_code_background.unwrap_or(theme.code_wash)
}
/// Rounded-wash geometry: small radius on a slightly inset box (paint-only —
/// x extends 4px past the glyphs, y insets 2px from the 22px line box).
pub const INLINE_CODE_RADIUS: f32 = 4.5;
pub const INLINE_CODE_PAD_X: f32 = 4.0;
pub const INLINE_CODE_INSET_Y: f32 = 2.0;
/// A thin visual margin between an inline-code pill and neighboring prose.
const INLINE_CODE_MARGIN: &str = "\u{2009}";

/// Flatten inline runs into shaped-text inputs. Pure given a theme.
pub fn flatten_runs(runs: &[InlineRun], theme: &Theme, bold_default: bool) -> FlatText {
    flatten_runs_weighted(
        runs,
        theme,
        if bold_default {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        },
    )
}

/// [`flatten_runs`] with an explicit base weight (table headers are 700 per
/// zeron's `table.headerWeight`; strong runs never drop below semibold).
fn flatten_runs_weighted(runs: &[InlineRun], theme: &Theme, base_weight: FontWeight) -> FlatText {
    let mut text = String::new();
    let mut out: Vec<TextRun> = Vec::with_capacity(runs.len());
    let mut links: Vec<(Range<usize>, String)> = Vec::new();
    let mut code_ranges: Vec<Range<usize>> = Vec::new();
    let mut previous_was_code = None;
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        // CSS-like separation without making the wash itself larger: a thin
        // space sits outside the code range at each prose/code boundary.
        if previous_was_code.is_some_and(|was_code| was_code != run.style.code) {
            text.push_str(INLINE_CODE_MARGIN);
            let mut margin_font = font(theme.font_sans.clone());
            margin_font.weight = base_weight;
            out.push(TextRun {
                len: INLINE_CODE_MARGIN.len(),
                font: margin_font,
                color: theme.text,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        let start = text.len();
        text.push_str(&run.text);
        let mut f = if run.style.code {
            font(theme.font_mono.clone())
        } else {
            font(theme.font_sans.clone())
        };
        f.weight = if run.style.bold && base_weight.0 < FontWeight::SEMIBOLD.0 {
            FontWeight::SEMIBOLD
        } else {
            base_weight
        };
        f.style = if run.style.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        // Monochrome by default; a scoped chat theme may override link color.
        let is_link = run.style.link.is_some();
        // Inline code reads emerald (see `inline_code_text`); everything else
        // stays the monochrome foreground.
        let color = if run.style.code {
            inline_code_text(theme)
        } else if is_link {
            theme.markdown_link.unwrap_or(theme.text)
        } else {
            theme.text
        };
        if run.style.code {
            // Merge adjacent code runs into one wash box (like links below).
            match code_ranges.last_mut() {
                Some(range) if range.end == start => range.end = text.len(),
                _ => code_ranges.push(start..text.len()),
            }
        }
        if let Some(url) = &run.style.link {
            // A still-streaming link (mend.rs sentinel) keeps link styling —
            // so the URL's completion changes nothing visually — but is not
            // clickable until the real destination exists.
            if url != super::mend::PENDING_LINK_URL {
                // Merge adjacent runs of the same link into one clickable range.
                match links.last_mut() {
                    Some((range, last_url)) if range.end == start && last_url == url => {
                        range.end = text.len();
                    }
                    _ => links.push((start..text.len(), url.clone())),
                }
            }
        }
        out.push(TextRun {
            len: run.text.len(),
            font: f,
            color,
            // Inline code's wash is painted as ROUNDED quads by the canvas
            // underlay (`code_wash_underlay`) — a run background here could
            // only be a square box.
            background_color: None,
            underline: is_link.then_some(UnderlineStyle {
                color: Some(theme.markdown_link.unwrap_or(theme.text_muted)),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: run.style.strikethrough.then_some(gpui::StrikethroughStyle {
                thickness: px(1.0),
                color: Some(theme.text_muted),
            }),
        });
        previous_was_code = Some(run.style.code);
    }
    FlatText {
        text: text.into(),
        runs: out,
        links,
        code_ranges,
    }
}

/// Flatten through the cross-frame cache when one is wired: settled blocks
/// reuse text + runs untouched (O(1) per block per frame); only blocks the
/// incremental parser invalidated rebuild.
fn flatten_cached(
    runs: &[InlineRun],
    base_weight: FontWeight,
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
) -> Rc<FlatText> {
    match &opts.cache {
        Some(cache) => {
            let mut cache = cache.borrow_mut();
            cache.sync_palette(theme);
            cache
                .flats
                .entry((opts.row_key.clone(), top_ix, ix))
                .or_insert_with(|| Rc::new(flatten_runs_weighted(runs, theme, base_weight)))
                .clone()
        }
        None => Rc::new(flatten_runs_weighted(runs, theme, base_weight)),
    }
}

/// Veiled, clickable text for a flattened block (no sizing wrapper).
fn flat_text_element(
    flat: &FlatText,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
) -> AnyElement {
    // Streaming veil: opacity-only recolor of the runs covering newly appended
    // chunks. Same text, same fonts, same lengths — layout is untouched.
    // Settled elements return no spans and reuse the cached runs unsplit.
    let text_runs = match &opts.veil {
        Some(veil) => {
            let spans = veil.borrow_mut().advance(ix, &flat.text, opts.now);
            apply_veil(flat.runs.clone(), &spans)
        }
        None => flat.runs.clone(),
    };
    let styled = StyledText::new(flat.text.clone()).with_runs(text_runs);
    let layout = styled.layout().clone();
    let text_el: AnyElement = if flat.links.is_empty() {
        styled.into_any_element()
    } else {
        let (ranges, urls): (Vec<_>, Vec<_>) = flat.links.iter().cloned().unzip();
        let id: SharedString = format!("{}-t{ix}", opts.row_key).into();
        InteractiveText::new(id, styled)
            .on_click(ranges, move |clicked_ix, _window, cx| {
                if let Some(url) = urls.get(clicked_ix) {
                    cx.open_url(url);
                }
            })
            .into_any_element()
    };
    // Underlay canvas: inline-code washes + the selection wash, painted
    // BEFORE the text (earlier sibling ⇒ underneath), reading glyph geometry
    // from the text's own layout handle. Pure paint — never in layout. The
    // same paint pass re-registers the frame-scoped window mouse listeners
    // that drive text selection (round 18; see markdown/selection.rs).
    let sel_key: std::sync::Arc<str> = format!("{}-t{ix}", opts.row_key).into();
    let code_ranges = flat.code_ranges.clone();
    let flat_text = flat.text.clone();
    let wash = inline_code_wash(theme);
    let sel_wash = selection_wash(theme);
    let selection = opts.selection.clone();
    let scope = opts.scope;
    let underlay = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            for range in &code_ranges {
                for rect in range_rects(&layout, range, INLINE_CODE_PAD_X, INLINE_CODE_INSET_Y) {
                    window.paint_quad(quad(
                        rect,
                        px(INLINE_CODE_RADIUS),
                        wash,
                        px(0.0),
                        gpui::transparent_black(),
                        BorderStyle::default(),
                    ));
                }
            }
            if let Some(range) = super::selection::wash_range(scope, &sel_key) {
                for rect in range_rects(&layout, &range, 0.0, 0.0) {
                    window.paint_quad(quad(
                        rect,
                        px(0.0),
                        sel_wash,
                        px(0.0),
                        gpui::transparent_black(),
                        BorderStyle::default(),
                    ));
                }
            }
            // Register this element into the frame's document-ordered
            // registry (paint order IS document order), then the frame's
            // mouse listeners.
            REGISTRY.with(|r| {
                r.borrow_mut().entry(scope).or_default().push(RegEntry {
                    key: sel_key.clone(),
                    text: flat_text.clone(),
                    layout: layout.clone(),
                })
            });
            register_selection_listeners(window, scope, &sel_key, &flat_text, &layout, selection);
        },
    )
    .absolute()
    .size_full();
    div()
        .relative()
        .child(underlay)
        .child(text_el)
        .into_any_element()
}

/// Selection tint: the accent hue under the glyphs, dark-panel strength.
fn selection_wash(theme: &Theme) -> Hsla {
    theme.accent.opacity(0.35) // indigo-400
}

/// Selection support for a plain (non-markdown) text element — the user
/// bubble. Paints the selection wash under the glyphs, registers the element
/// into the frame's document-ordered registry (so drags span into adjacent
/// markdown rows and Cmd+C joins in order), and re-registers the mouse
/// listeners. Call from a paint-phase canvas that sits UNDER the text.
pub(crate) fn paint_text_selection(
    window: &mut Window,
    scope: super::selection::SelectionScope,
    key: &std::sync::Arc<str>,
    text: &SharedString,
    layout: &gpui::TextLayout,
    theme: &Theme,
    selection: Option<SelectionUi>,
) {
    if let Some(range) = super::selection::wash_range(scope, key) {
        for rect in range_rects(layout, &range, 0.0, 0.0) {
            window.paint_quad(quad(
                rect,
                px(0.0),
                selection_wash(theme),
                px(0.0),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }
    REGISTRY.with(|r| {
        r.borrow_mut().entry(scope).or_default().push(RegEntry {
            key: key.clone(),
            text: text.clone(),
            layout: layout.clone(),
        })
    });
    register_selection_listeners(window, scope, key, text, layout, selection);
}

/// One painted text element, registered per frame in document order — the
/// continuity model that lets a drag span paragraphs/list items (Zed gets
/// this for free from its single-element markdown; our tree rebuilds it).
struct RegEntry {
    key: std::sync::Arc<str>,
    text: SharedString,
    layout: gpui::TextLayout,
}

thread_local! {
    static REGISTRY: RefCell<
        std::collections::HashMap<super::selection::SelectionScope, Vec<RegEntry>>
    > = RefCell::new(std::collections::HashMap::new());
}

/// A zero-size canvas that clears `scope`'s selection registry — paint it
/// FIRST in the surface's root (before any selectable text), so each frame's
/// registry holds exactly that frame's visible text elements in paint order.
pub fn selection_frame_reset(scope: super::selection::SelectionScope) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, _, _| {
            REGISTRY.with(|r| {
                r.borrow_mut().remove(&scope);
            });
        },
    )
    .absolute()
    .w(px(0.0))
    .h(px(0.0))
}

/// `(element index, byte offset)` for a window position: prefer the registered
/// element whose full 2-D bounds contain it, else the nearest bounds. The X
/// dimension matters for tables, whose cells share the same vertical band.
fn registry_point(
    scope: super::selection::SelectionScope,
    position: gpui::Point<gpui::Pixels>,
) -> Option<(usize, usize)> {
    REGISTRY.with(|r| {
        let binding = r.borrow();
        let reg = binding.get(&scope).map_or(&[][..], |v| v.as_slice());
        let mut best: Option<(usize, f32)> = None;
        for (ei, entry) in reg.iter().enumerate() {
            let b = entry.layout.bounds();
            let dx = if position.x < b.left() {
                f32::from(b.left() - position.x)
            } else if position.x > b.right() {
                f32::from(position.x - b.right())
            } else {
                0.0
            };
            let dy = if position.y < b.top() {
                f32::from(b.top() - position.y)
            } else if position.y > b.bottom() {
                f32::from(position.y - b.bottom())
            } else {
                0.0
            };
            let distance_sq = dx * dx + dy * dy;
            if best.is_none_or(|(_, d)| distance_sq < d) {
                best = Some((ei, distance_sq));
            }
            if distance_sq == 0.0 {
                break;
            }
        }
        let (ei, _) = best?;
        let ix = match reg[ei].layout.index_for_position(position) {
            Ok(ix) | Err(ix) => ix,
        };
        Some((ei, ix))
    })
}

/// Current window-space endpoint for a settled selection head. The transcript
/// uses this to keep its floating Comment affordance attached while scrolling,
/// streaming, or resize/reflow moves the underlying text.
pub(crate) fn selection_anchor(
    scope: super::selection::SelectionScope,
    key: &str,
    index: usize,
) -> Option<gpui::Point<gpui::Pixels>> {
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let entry = registry
            .get(&scope)
            .and_then(|v| v.iter().find(|entry| entry.key.as_ref() == key))?;
        entry
            .layout
            .position_for_index(index)
            .map(|position| position + point(px(0.0), entry.layout.line_height()))
    })
}

/// Resolve the anchor + head into document-ordered spans over the frame's
/// registry and store them; true if the selection changed.
fn resolve_drag(
    scope: super::selection::SelectionScope,
    anchor_key: &str,
    anchor_ix: usize,
    head: (usize, usize),
) -> bool {
    REGISTRY.with(|r| {
        let reg = r.borrow();
        let Some(reg) = reg.get(&scope) else {
            return false;
        };
        let Some(anchor_ei) = reg.iter().position(|e| e.key.as_ref() == anchor_key) else {
            return false; // anchor scrolled out of the frame — keep spans
        };
        let elements: Vec<(&str, &str)> = reg
            .iter()
            .map(|e| (e.key.as_ref(), e.text.as_ref()))
            .collect();
        // The head element under the mouse (falls back to the anchor if the
        // pointer ran past the registry edge — the spans stay valid).
        let head_key: String = reg
            .get(head.0)
            .map(|e| e.key.to_string())
            .unwrap_or_else(|| anchor_key.to_string());
        let spans = if matches!(scope, super::selection::SelectionScope::Changes(_)) {
            super::selection::resolve_line_spans(&elements, (anchor_ei, anchor_ix), head)
        } else {
            super::selection::resolve_spans(&elements, (anchor_ei, anchor_ix), head)
        };
        super::selection::update_drag(scope, &head_key, head.1, spans)
    })
}

/// Register this frame's window-level mouse listeners for one text element's
/// selection (Zed-markdown mechanics: window-level so a drag keeps tracking
/// outside the element's bounds; frame-scoped, so paint re-registers).
/// `selection` threads the surface's Comment-pill callbacks through.
fn register_selection_listeners(
    window: &mut Window,
    scope: super::selection::SelectionScope,
    key: &std::sync::Arc<str>,
    text: &SharedString,
    layout: &gpui::TextLayout,
    selection: Option<SelectionUi>,
) {
    use gpui::{DispatchPhase, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
    // A long (or horizontally scrolled) line may lay out beyond its column.
    // Only its visible clipped area can start a selection; otherwise a click
    // on the other split-diff column would activate both sides' listeners.
    let visible_bounds = layout.bounds().intersect(&window.content_mask().bounds);
    {
        let (key, text, layout) = (key.clone(), text.clone(), layout.clone());
        let selection = selection.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || e.button != MouseButton::Left {
                return;
            }
            if visible_bounds.contains(&e.position) {
                let ix = match layout.index_for_position(e.position) {
                    Ok(ix) | Err(ix) => ix,
                };
                match e.click_count {
                    2 => {
                        let range = super::selection::word_range(&text, ix);
                        super::selection::begin_with_span(scope, &key, &text, range);
                    }
                    n if n >= 3 => {
                        super::selection::begin_with_span(scope, &key, &text, 0..text.len());
                    }
                    _ => super::selection::begin(scope, &key, ix),
                }
                if let Some(sel) = &selection {
                    (sel.on_started)(window, cx);
                }
                window.refresh();
            } else if super::selection::clear_if_owner(scope, &key) {
                if let Some(sel) = &selection {
                    (sel.on_cleared)(window, cx);
                }
                window.refresh();
            }
        });
    }
    {
        let key = key.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _cx| {
            if phase != DispatchPhase::Bubble || !e.dragging() {
                return;
            }
            // Only the anchor element's listener drives the drag.
            let Some(anchor_ix) = super::selection::drag_anchor(scope, &key) else {
                return;
            };
            if super::selection::drag_is_fixed(scope, &key) {
                return;
            }
            let Some(head) = registry_point(scope, e.position) else {
                return;
            };
            if resolve_drag(scope, &key, anchor_ix, head) {
                window.refresh();
            }
        });
    }
    {
        let key = key.clone();
        let selection = selection.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            // Mouse-up can arrive without a final MouseMove. Resolve its exact
            // point once more for a simple drag so the release glyph/row is
            // included. Fixed double/triple-click spans stay intact.
            if !super::selection::drag_is_fixed(scope, &key)
                && let Some(anchor_ix) = super::selection::drag_anchor(scope, &key)
                && let Some(head) = registry_point(scope, e.position)
            {
                resolve_drag(scope, &key, anchor_ix, head);
            }
            if let Some(snapshot) = super::selection::end_drag(scope, &key) {
                // X11 middle-click paste parity (Zed does the same).
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                cx.write_to_primary(gpui::ClipboardItem::new_string(snapshot.text.clone()));
                if let Some(sel) = &selection {
                    (sel.on_settled)(snapshot, e.position, window, cx);
                }
            }
        });
    }
}

/// The wash boxes for one byte range: one box per visual line the range
/// covers (soft wraps split it), in window coordinates from the laid-out
/// text's own geometry. `pad_x` overhangs the box horizontally (inline code);
/// `inset_y` shrinks it vertically — both 0 for a selection wash, which wants
/// full-line-height boxes that tile seamlessly across wrapped rows.
pub(crate) fn range_rects(
    layout: &gpui::TextLayout,
    range: &Range<usize>,
    pad_x: f32,
    inset_y: f32,
) -> Vec<Bounds<gpui::Pixels>> {
    range_rects_with_positions(
        layout.bounds(),
        layout.line_height(),
        range,
        pad_x,
        inset_y,
        |ix| layout.position_for_index(ix),
    )
}

/// Split a decorated range into one rectangle per visual line.
///
/// GPUI reports a soft-wrap boundary with upstream affinity: the boundary
/// index is still positioned at the end of the preceding line, while the next
/// byte is positioned on the continuation line. Use that downstream position
/// when the range begins at the boundary, otherwise the first continuation
/// glyph is omitted from the wash/chip background.
fn range_rects_with_positions(
    bounds: Bounds<gpui::Pixels>,
    line_height: gpui::Pixels,
    range: &Range<usize>,
    pad_x: f32,
    inset_y: f32,
    position_for_index: impl Fn(usize) -> Option<gpui::Point<gpui::Pixels>>,
) -> Vec<Bounds<gpui::Pixels>> {
    let mut rects = Vec::new();
    let mut cur = range.start;
    // Walk the range one visual row at a time: find the furthest index that
    // still sits on the current row (binary search over glyph positions).
    let mut guard = 0;
    while cur < range.end && guard < 256 {
        guard += 1;
        let Some(mut p1) = position_for_index(cur) else {
            break;
        };
        // A range that starts exactly at a soft-wrap boundary belongs to the
        // continuation line, not the final pixel of the preceding line.
        if let Some(after) = position_for_index(cur.saturating_add(1))
            && after.y > p1.y
        {
            p1 = gpui::point(bounds.left(), after.y);
        }
        // `seg_end` closes the wash on this row; `next` is the first index on
        // the following row (strict progress even though a row-end index's
        // position still reports the earlier row).
        let (seg_end, next) = match position_for_index(range.end) {
            Some(pe) if pe.y == p1.y => (range.end, range.end),
            _ => {
                // Largest ix on this row (probes stay on char boundaries only
                // at the ends; intermediate probes just need a y).
                let (mut lo, mut hi) = (cur, range.end);
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    match position_for_index(mid) {
                        Some(pm) if pm.y == p1.y => lo = mid,
                        _ => hi = mid,
                    }
                }
                // Keep the boundary index as the next iteration's start.
                // GPUI gives it upstream affinity, so the next iteration can
                // apply the downstream correction above and include the first
                // glyph on the continuation line.
                (lo, lo)
            }
        };
        if let Some(p2) = position_for_index(seg_end)
            && p2.x > p1.x
        {
            rects.push(Bounds::new(
                point(p1.x - px(pad_x), p1.y + px(inset_y)),
                size(
                    p2.x - p1.x + px(2.0 * pad_x),
                    line_height - px(2.0 * inset_y),
                ),
            ));
        }
        if next <= cur {
            break;
        }
        cur = next;
    }
    rects
}

#[allow(clippy::too_many_arguments)]
fn text_element(
    runs: &[InlineRun],
    size: f32,
    line_height: f32,
    bold_default: bool,
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
) -> AnyElement {
    let weight = if bold_default {
        FontWeight::SEMIBOLD
    } else {
        FontWeight::NORMAL
    };
    let flat = flatten_cached(runs, weight, top_ix, ix, opts, theme);
    let inner = flat_text_element(&flat, ix, opts, theme);
    div()
        .text_size(px(size))
        .line_height(px(line_height))
        .child(inner)
        .into_any_element()
}

pub fn code_block_background(theme: &Theme) -> Hsla {
    theme
        .code_block_background
        .unwrap_or_else(|| theme.ink(0.035))
}

/// Scoped to fenced blocks: inline code, tool/diff renderers and the installed
/// theme keep their original colors. Neutral syntax tokens use the base code
/// foreground; keywords, strings, comments and other colored tokens do not.
fn code_block_theme(theme: &Theme) -> Theme {
    let mut code = theme.clone();
    if let Some(foreground) = theme.code_block_text {
        code.text = foreground;
        code.syntax.variable = foreground;
        code.syntax.parameter = foreground;
        code.syntax.operator = foreground;
        code.syntax.punctuation = foreground;
    }
    code
}

#[allow(clippy::too_many_arguments)]
fn render_code_block(
    language: Option<&str>,
    code: &str,
    top_ix: usize,
    ix: usize,
    opts: &RenderOptions,
    theme: &Theme,
    highlight: CodeHighlight,
) -> AnyElement {
    let mono = font(theme.font_mono.clone());
    let code_theme = code_block_theme(theme);
    let chrome_color = theme.code_block_text.unwrap_or(theme.text_muted);
    // Per-line strings + runs through the cross-frame cache (validity: code
    // length + highlight slice identity — a fresh highlight Arc re-derives).
    let hl_key = highlight.map_or((0, 0), |h| (h.as_ptr() as usize, h.len()));
    let build = || {
        let lines: Vec<(SharedString, Vec<TextRun>)> = code
            .split('\n')
            .enumerate()
            .map(|(li, line)| {
                let spans = highlight
                    .and_then(|h| h.get(li))
                    .map(|t| &t[..])
                    .unwrap_or(&[]);
                (
                    SharedString::from(line.to_string()),
                    runs_for_syntax_line(line, spans, &mono, &code_theme),
                )
            })
            .collect();
        Rc::new(CachedCode {
            code_len: code.len(),
            hl_key,
            lines,
        })
    };
    let cached: Rc<CachedCode> = match &opts.cache {
        Some(cache) => {
            let mut cache = cache.borrow_mut();
            cache.sync_palette(theme);
            let entry = cache
                .code
                .entry((opts.row_key.clone(), top_ix, ix))
                .or_insert_with(&build);
            if entry.code_len != code.len() || entry.hl_key != hl_key {
                *entry = build();
            }
            entry.clone()
        }
        None => build(),
    };
    // Streaming veil over appended code, tracked on the whole code text and
    // sliced per line below (paint-only run recolor — heights stay exact).
    let veil_spans = match &opts.veil {
        Some(veil) => veil.borrow_mut().advance(ix, code, opts.now),
        None => Vec::new(),
    };
    let scroll_id: SharedString = format!("{}-code{ix}", opts.row_key).into();
    // Copy affordance (round 9; no source counterpart — the original block is
    // header + body only): a small ghost button in the block's top-right,
    // absolutely overlaid so clicking / the "Copied" flash never shifts
    // layout. Sits centered in the header when there is one, floats over the
    // first code line otherwise.
    let copy_button = opts.copy.clone().map(|copy| {
        let copied = copy.copied_ix == Some(ix);
        let code_text: SharedString = code.to_string().into();
        let handler = copy.handler.clone();
        let fade_key = format!("{}-copy{ix}", opts.row_key);
        div()
            .id(SharedString::from(fade_key.clone()))
            .absolute()
            .top(px(3.0))
            .right(px(5.0))
            .h(px(20.0))
            .px(px(6.0))
            .rounded(px(5.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            // Ghost-button hover wash fades over transition-colors like every
            // other interactive chrome (crate::motion hover fades).
            .bg(crate::motion::hover_blend(
                &fade_key,
                gpui::transparent_black(),
                theme.ink(0.08),
            ))
            .on_hover(crate::motion::hover_listener(fade_key))
            .text_size(px(10.5))
            .text_color(chrome_color)
            .on_click(move |_, window, cx| handler(ix, code_text.clone(), window, cx))
            .child(
                crate::icons::icon(if copied {
                    crate::icons::CHECK
                } else {
                    crate::icons::COPY
                })
                .size(px(12.0))
                .text_color(chrome_color),
            )
            .when(copied, |el| el.child(SharedString::from("Copied")))
    });
    div()
        .rounded(px(10.0))
        // Faint white wash over the near-black panel ≈ #101010 (zeron's code
        // surface), with the hairline border.
        .bg(code_block_background(theme))
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .relative()
        .when_some(language, |el, lang| {
            el.child(
                div()
                    .px(px(CODE_PADDING_X))
                    .py(px(5.0))
                    .border_b_1()
                    .border_color(theme.border)
                    // A whisper of tone separation between header and body.
                    .bg(theme.ink(0.02))
                    .text_size(px(11.0))
                    .text_color(chrome_color)
                    .child(SharedString::from(lang.to_string())),
            )
        })
        .child(
            div()
                .id(scroll_id)
                .overflow_x_scroll()
                .px(px(CODE_PADDING_X))
                .py(px(CODE_PADDING_Y))
                .font_family(theme.font_mono.clone())
                .text_size(px(theme.markdown.code_size))
                .line_height(px(theme.markdown.code_line_height))
                .whitespace_nowrap()
                .flex()
                .flex_col()
                .children((0..cached.lines.len()).scan(0usize, move |off, li| {
                    let (line, runs) = &cached.lines[li];
                    let start = *off;
                    *off = start + line.len() + 1; // +1 for the '\n'
                    let local = slice_spans(&veil_spans, start, start + line.len());
                    let runs = apply_veil(runs.clone(), &local);
                    Some(
                        div()
                            .h(px(theme.markdown.code_line_height))
                            .flex_none()
                            .child(StyledText::new(line.clone()).with_runs(runs)),
                    )
                })),
        )
        // Overlay LAST so it paints above the header/body.
        .children(copy_button)
        .into_any_element()
}

/// Paint color for a token class — the soft syntax palette (round 9: the
/// original's mdTheme code blocks are monochrome `#e7e7e7`, but the user
/// asked for color; these are the diff pane's hues, now shared by both).
pub fn token_color(kind: HighlightKind, theme: &Theme) -> Hsla {
    theme.syntax.color(kind)
}

/// Build the exact-cover `TextRun` list for one code line from its tokens.
/// Same font everywhere — recoloring can never change layout.
/// Build paint-only runs from the neutral Tree-sitter contract.
pub fn runs_for_syntax_line(
    line: &str,
    spans: &[HighlightSpan],
    mono: &gpui::Font,
    theme: &Theme,
) -> Vec<TextRun> {
    runs_for_syntax_line_with_plain(line, spans, mono, theme.text, theme)
}

pub fn runs_for_syntax_line_with_plain(
    line: &str,
    spans: &[HighlightSpan],
    mono: &gpui::Font,
    plain_color: Hsla,
    theme: &Theme,
) -> Vec<TextRun> {
    let plain = |len: usize| TextRun {
        len,
        font: mono.clone(),
        color: plain_color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut at = 0usize;
    for span in spans {
        if span.range.start > at {
            runs.push(plain(span.range.start - at));
        }
        let mut run = plain(span.range.len());
        run.color = token_color(span.kind, theme);
        runs.push(run);
        at = span.range.end;
    }
    if at < line.len() {
        runs.push(plain(line.len() - at));
    }
    runs.retain(|run| run.len > 0);
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parser::InlineStyle;

    #[test]
    fn clipped_long_line_cannot_start_selection_in_the_other_diff_column() {
        let text = Bounds::new(point(px(-80.0), px(0.0)), gpui::size(px(900.0), px(21.0)));
        let left_column = Bounds::new(point(px(0.0), px(0.0)), gpui::size(px(200.0), px(300.0)));
        let visible = text.intersect(&left_column);
        assert!(text.contains(&point(px(250.0), px(10.0))));
        assert!(!visible.contains(&point(px(250.0), px(10.0))));
        assert!(visible.contains(&point(px(100.0), px(10.0))));
    }
    #[test]
    fn inline_colors_preserve_prose_fenced_code_and_default_washes() {
        for mut theme in [Theme::dark(), Theme::light()] {
            assert_eq!(inline_code_text(&theme), theme.code_text);
            assert_eq!(inline_code_wash(&theme), theme.code_wash);
            let plain_code_color = code_block_theme(&theme).text;
            let fenced_background = code_block_background(&theme);
            let foreground = gpui::rgb(0xaa2266).into();
            let background = gpui::rgb(0xeeeedd).into();
            theme.inline_code_text = Some(foreground);
            theme.inline_code_background = Some(background);
            assert_eq!(inline_code_text(&theme), foreground);
            assert_eq!(inline_code_wash(&theme), background);
            assert_eq!(code_block_theme(&theme).text, plain_code_color);
            assert_eq!(code_block_background(&theme), fenced_background);
            let code_run = InlineRun {
                text: "inline".into(),
                style: InlineStyle {
                    code: true,
                    ..Default::default()
                },
            };
            let flat = flatten_runs(&[code_run], &theme, false);
            assert_eq!(flat.runs[0].color, foreground);
            assert_eq!(flat.code_ranges, vec![0..6]);
            let prose = flatten_runs(
                &[InlineRun {
                    text: "prose".into(),
                    style: InlineStyle::default(),
                }],
                &theme,
                false,
            );
            assert_eq!(prose.runs[0].color, theme.text);
        }
    }

    #[test]
    fn inline_color_changes_refresh_cached_runs() {
        let cache = Rc::new(RefCell::new(RenderCache::default()));
        let mut opts = RenderOptions::settled("inline-color-cache".into());
        opts.cache = Some(cache);
        let runs = [InlineRun {
            text: "value".into(),
            style: InlineStyle {
                code: true,
                ..Default::default()
            },
        }];
        let mut theme = Theme::dark();
        theme.text_style_revision = 1;
        let before = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &theme);
        theme.inline_code_text = Some(gpui::rgb(0xffcc88).into());
        theme.inline_code_background = Some(gpui::rgb(0x332200).into());
        theme.text_style_revision = 2;
        let after = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &theme);
        assert!(!Rc::ptr_eq(&before, &after));
        assert_eq!(after.runs[0].color, theme.inline_code_text.unwrap());
        assert_eq!(
            before.code_ranges, after.code_ranges,
            "color changes must not alter layout"
        );
        assert_eq!(before.text, after.text);
    }

    #[test]
    fn code_block_base_color_preserves_colored_syntax_and_other_renderers() {
        for mut theme in [Theme::dark(), Theme::light()] {
            assert_eq!(code_block_background(&theme), theme.ink(0.035));
            let foreground = gpui::rgb(0x6655aa).into();
            theme.code_block_text = Some(foreground);
            theme.code_block_background = Some(gpui::rgb(0xf0f0f0).into());
            let code = code_block_theme(&theme);
            assert_eq!(
                code_block_background(&theme),
                theme.code_block_background.unwrap()
            );
            for kind in [
                HighlightKind::Variable,
                HighlightKind::Parameter,
                HighlightKind::Operator,
                HighlightKind::Punctuation,
                HighlightKind::Embedded,
            ] {
                assert_eq!(token_color(kind, &code), foreground);
                assert_eq!(token_color(kind, &theme), theme.text);
            }
            for kind in [
                HighlightKind::Keyword,
                HighlightKind::String,
                HighlightKind::Comment,
                HighlightKind::Number,
                HighlightKind::Function,
                HighlightKind::Type,
                HighlightKind::Boolean,
                HighlightKind::VariableSpecial,
            ] {
                assert_eq!(token_color(kind, &code), token_color(kind, &theme));
            }
            let spans = [
                HighlightSpan {
                    range: 0..3,
                    kind: HighlightKind::Keyword,
                },
                HighlightSpan {
                    range: 4..7,
                    kind: HighlightKind::Variable,
                },
                HighlightSpan {
                    range: 8..11,
                    kind: HighlightKind::String,
                },
            ];
            let mono = font(theme.font_mono.clone());
            let runs = runs_for_syntax_line("abc def ghi", &spans, &mono, &code);
            assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), 11);
            assert_eq!(
                runs.iter().map(|r| r.color).collect::<Vec<_>>(),
                vec![
                    theme.syntax.keyword,
                    foreground,
                    foreground,
                    foreground,
                    theme.syntax.string,
                ]
            );
            let ordinary = runs_for_syntax_line("plain", &[], &mono, &theme);
            assert_eq!(
                ordinary[0].color, theme.text,
                "generic/diff rendering must not use the fenced override"
            );
            assert_eq!(inline_code_text(&code), inline_code_text(&theme));
        }
    }

    #[test]
    fn code_color_change_rebuilds_cached_code_runs() {
        let cache = Rc::new(RefCell::new(RenderCache::default()));
        let mut opts = RenderOptions::settled("code-color-cache".into());
        opts.cache = Some(cache.clone());
        let mut theme = Theme::dark();
        theme.text_style_revision = 1;
        theme.code_block_text = Some(gpui::rgb(0xffccaa).into());
        let _ = render_code_block(None, "plain", 0, 0, &opts, &theme, None);
        let key = (opts.row_key.clone(), 0, 0);
        let before = cache.borrow().code[&key].clone();
        theme.text_style_revision = 2;
        theme.code_block_text = Some(gpui::rgb(0xaaccff).into());
        let _ = render_code_block(None, "plain", 0, 0, &opts, &theme, None);
        let after = cache.borrow().code[&key].clone();
        assert!(!Rc::ptr_eq(&before, &after));
        assert_eq!(before.lines[0].1[0].color, gpui::rgb(0xffccaa).into());
        assert_eq!(after.lines[0].1[0].color, theme.code_block_text.unwrap());
    }

    #[test]
    fn chat_style_revision_invalidates_cached_fonts_and_colors() {
        let cache = Rc::new(RefCell::new(RenderCache::default()));
        let mut opts = RenderOptions::settled("style-cache".into());
        opts.cache = Some(cache.clone());
        let runs = vec![InlineRun {
            text: "link".into(),
            style: InlineStyle {
                link: Some("https://example.com".into()),
                ..Default::default()
            },
        }];
        let mut old = Theme::dark();
        old.text_style_revision = 1;
        let before = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &old);
        let mut new = old.clone();
        new.text_style_revision = 2;
        new.font_sans = "Test Serif".into();
        new.markdown_link = Some(gpui::rgb(0x3388cc).into());
        let after = flatten_cached(&runs, FontWeight::NORMAL, 0, 0, &opts, &new);
        assert!(!Rc::ptr_eq(&before, &after));
        assert_eq!(after.runs[0].font.family.as_ref(), "Test Serif");
        assert_eq!(after.runs[0].color, new.markdown_link.unwrap());
        assert_eq!(before.runs[0].font.family, old.font_sans);
    }

    #[test]
    fn code_line_runs_cover_exactly() {
        let theme = Theme::dark();
        let mono = font(theme.font_mono.clone());
        let line = r#"let x = "hi"; // done"#;
        let document = cypher_syntax::highlight(cypher_syntax::HighlightRequest {
            source: line,
            path: None,
            fence_tag: Some("rust"),
        })
        .unwrap();
        let runs = runs_for_syntax_line(line, &document.lines[0], &mono, &theme);
        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, line.len());
        assert!(
            runs.iter().all(|r| r.font == mono),
            "highlight must not change fonts"
        );
        // At least one non-plain color made it through.
        assert!(runs.iter().any(|r| r.color != theme.text));
    }

    #[test]
    fn tree_sitter_runs_are_rich_and_paint_only() {
        let theme = Theme::dark();
        let mono = font(theme.font_mono.clone());
        let line = "let widget = build!(42);";
        let document = cypher_syntax::highlight(cypher_syntax::HighlightRequest {
            source: line,
            path: None,
            fence_tag: Some("rust"),
        })
        .unwrap();
        let runs = runs_for_syntax_line(line, &document.lines[0], &mono, &theme);
        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), line.len());
        assert!(runs.iter().all(|run| run.font == mono));
        let colors = runs.iter().map(|run| run.color).collect::<Vec<_>>();
        assert!(colors.contains(&theme.syntax.keyword));
        assert!(colors.contains(&theme.syntax.macro_name));
        assert!(colors.contains(&theme.syntax.number));
    }

    #[test]
    fn code_line_runs_with_no_tokens_are_one_plain_run() {
        let theme = Theme::dark();
        let mono = font(theme.font_mono.clone());
        let runs = runs_for_syntax_line("plain text", &[], &mono, &theme);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 10);
    }

    #[test]
    fn flatten_collects_and_merges_inline_code_ranges() {
        let theme = Theme::dark();
        let code = |text: &str| InlineRun {
            text: text.into(),
            style: InlineStyle {
                code: true,
                ..Default::default()
            },
        };
        let plain = |text: &str| InlineRun {
            text: text.into(),
            style: InlineStyle::default(),
        };
        let flat = flatten_runs(
            &[
                plain("use "),
                code("foo"),
                code("()"),
                plain(" and "),
                code("bar"),
            ],
            &theme,
            false,
        );
        // Thin margins separate code from prose without entering the wash;
        // adjacent code runs still merge into ONE box.
        assert_eq!(flat.text, "use \u{2009}foo()\u{2009} and \u{2009}bar");
        assert_eq!(flat.code_ranges, vec![7..12, 23..26]);
        // Code text is the emerald tint; the square run background is gone
        // (the rounded wash is painted by the canvas underlay instead).
        assert_eq!(flat.runs[2].color, inline_code_text(&theme));
        assert_eq!(flat.runs[2].background_color, None);
        assert_eq!(flat.runs[0].color, theme.text);
    }

    #[test]
    fn code_palette_is_colored_and_shared() {
        // Round 9: transcript code blocks paint the soft hues (rose keyword,
        // green string, amber number); comments stay faint neutral.
        let theme = Theme::dark();
        assert_ne!(token_color(HighlightKind::Keyword, &theme), theme.text);
        assert_ne!(
            token_color(HighlightKind::String, &theme),
            token_color(HighlightKind::Keyword, &theme)
        );
        assert_ne!(token_color(HighlightKind::Comment, &theme), theme.text);
    }

    #[test]
    fn flatten_runs_maps_links_and_styles() {
        let theme = Theme::dark();
        let runs = vec![
            InlineRun {
                text: "go ".into(),
                style: InlineStyle::default(),
            },
            InlineRun {
                text: "here".into(),
                style: InlineStyle {
                    link: Some("https://x.dev".into()),
                    ..Default::default()
                },
            },
            InlineRun {
                text: " now".into(),
                style: InlineStyle {
                    bold: true,
                    ..Default::default()
                },
            },
        ];
        let flat = flatten_runs(&runs, &theme, false);
        assert_eq!(flat.text, "go here now");
        assert_eq!(flat.links, vec![(3..7, "https://x.dev".to_string())]);
        let total: usize = flat.runs.iter().map(|r| r.len).sum();
        assert_eq!(total, flat.text.len());
        // Links stay monochrome (foreground + underline), never accent-tinted.
        assert_eq!(flat.runs[1].color, theme.text);
        assert!(flat.runs[1].underline.is_some());
        assert_eq!(flat.runs[2].font.weight, FontWeight::SEMIBOLD);
    }

    /// Model GPUI's upstream affinity at a soft-wrap boundary: byte 5 is
    /// reported at the end of row 0, while byte 6 is on row 1.
    fn wrapped_position(ix: usize) -> Option<gpui::Point<gpui::Pixels>> {
        (ix <= 9).then(|| {
            if ix <= 5 {
                point(px(ix as f32 * 10.0), px(0.0))
            } else {
                point(px((ix - 5) as f32 * 10.0), px(22.0))
            }
        })
    }

    fn wrapped_range_rects(range: Range<usize>) -> Vec<Bounds<gpui::Pixels>> {
        range_rects_with_positions(
            Bounds::new(point(px(0.0), px(0.0)), size(px(50.0), px(44.0))),
            px(22.0),
            &range,
            0.0,
            0.0,
            wrapped_position,
        )
    }

    #[test]
    fn range_starting_at_soft_wrap_includes_first_glyph() {
        let rects = wrapped_range_rects(5..9);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].origin, point(px(0.0), px(22.0)));
        assert_eq!(rects[0].size, size(px(40.0), px(22.0)));
    }

    #[test]
    fn range_crossing_soft_wrap_includes_first_continuation_glyph() {
        let rects = wrapped_range_rects(2..9);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].origin, point(px(20.0), px(0.0)));
        assert_eq!(rects[0].size, size(px(30.0), px(22.0)));
        assert_eq!(rects[1].origin, point(px(0.0), px(22.0)));
        assert_eq!(rects[1].size, size(px(40.0), px(22.0)));
    }

    #[test]
    fn table_columns_floor_and_padding() {
        // A short column keeps its content width (floored at MIN_COLUMN_CONTENT
        // + padding); a wide one may wrap but no narrower than minColumnWidth.
        let geo = table_columns(&[10.0, 200.0]);
        assert_eq!(geo.naturals, vec![72.0, 224.0]); // 48+24, 200+24
        assert_eq!(geo.minimums, vec![72.0, 96.0]);
        assert_eq!(geo.min_table_width, 168.0);
    }

    #[test]
    fn table_columns_are_content_proportional_not_equal() {
        let geo = table_columns(&[300.0, 60.0, 60.0]);
        // Flex grow factors are the naturals — a prose column gets a larger
        // share than short ones (not equal thirds).
        assert!(geo.naturals[0] > 3.0 * geo.naturals[1] * 0.9);
        assert_eq!(geo.naturals[1], geo.naturals[2]);
    }

    #[test]
    fn table_header_flattens_at_weight_700() {
        let theme = Theme::dark();
        let runs = vec![InlineRun {
            text: "Header".into(),
            style: InlineStyle::default(),
        }];
        let flat = flatten_runs_weighted(&runs, &theme, TABLE_HEADER_WEIGHT);
        assert_eq!(flat.runs[0].font.weight, FontWeight::BOLD);
        // Strong runs inside a 700 header stay 700 (never drop to semibold).
        let bold_runs = vec![InlineRun {
            text: "Strong".into(),
            style: InlineStyle {
                bold: true,
                ..Default::default()
            },
        }];
        let flat = flatten_runs_weighted(&bold_runs, &theme, TABLE_HEADER_WEIGHT);
        assert_eq!(flat.runs[0].font.weight, FontWeight::BOLD);
    }

    #[test]
    fn adjacent_same_link_runs_merge_into_one_range() {
        let theme = Theme::dark();
        let style = InlineStyle {
            link: Some("https://x.dev".into()),
            ..Default::default()
        };
        let runs = vec![
            InlineRun {
                text: "bold".into(),
                style: InlineStyle {
                    bold: true,
                    ..style.clone()
                },
            },
            InlineRun {
                text: " tail".into(),
                style,
            },
        ];
        let flat = flatten_runs(&runs, &theme, false);
        assert_eq!(flat.links, vec![(0..9, "https://x.dev".to_string())]);
    }
}
