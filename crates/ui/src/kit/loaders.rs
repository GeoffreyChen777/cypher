//! Loaders: the gradient matrix spinners, the running-tool arc, the pulsing
//! loading wash and the boot splash content. All motion routes through
//! `crate::kit::motion` pure helpers, so the math is unit-tested.
//!
//! Rendering pattern: a loader is a fixed-size element that paints its frame
//! at paint time from the one shared pulse clock ([`motion::pulse_delta`]),
//! deriving each cell's phase from it, so all loaders stay phase-locked and
//! never move surrounding layout. Inside a cached view's
//! [`LiveLayer`](crate::kit::live_layer::LiveLayer) a loader only records its
//! slot, and the layer paints its frames over the replayed view instead.

use std::f32::consts::TAU;

use gpui::{
    AnyElement, Bounds, EntityId, IntoElement, ParentElement, Pixels, SharedString, Styled,
    TransformationMatrix, Window, canvas, div, px,
};

use crate::kit::live_layer;
use crate::kit::motion::{
    self, CYPHER_PULSE, GRADIENT_SPIN, MotionSpec, PROGRESS_FLOW, SPLASH_OUT, STATUS_SPIN,
};
use crate::kit::progress_bar::ProgressBar;
use crate::kit::theme::{Appearance, MonoStyled, Theme, oklch};
use gpui::Hsla;

// Shared with the terminal viewport (`cypher_proto::motion`) so both animate the
// same loaders from the same numbers.
pub use cypher_proto::motion::MATRIX_SIDE;

pub use cypher_proto::motion::{GSPIN_DIM, GSPIN_ROW_TINTS};

/// Light-mode resting opacity for [`mini_gradient_spinner`] cells. At
/// [`GSPIN_DIM`] only the one or two pulsing cells showed on a light
/// surface, so the grid flickered instead of reading as a spinner; resting
/// brighter keeps its 2×3 shape between pulses.
const GSPIN_DIM_LIGHT: f32 = 0.3;

/// [`mini_gradient_spinner`] row tints for `appearance`. Dark keeps the
/// pastel sunrise ([`GSPIN_ROW_TINTS`]); light deepens the same hues, since
/// the pastel blue and peach sit at nearly the light frost's lightness and
/// vanished there (user report).
fn mini_row_tints(appearance: Appearance) -> [Hsla; 3] {
    match appearance {
        Appearance::Dark => GSPIN_ROW_TINTS.map(|tint| gpui::rgb(tint).into()),
        Appearance::Light => [
            oklch(0.62, 0.12, 250.0),
            oklch(0.68, 0.15, 55.0),
            oklch(0.63, 0.19, 10.0),
        ],
    }
}

/// One loader's look: what its element paints, and what a
/// [`LiveLayer`](crate::kit::live_layer::LiveLayer) slot replays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Loader {
    /// [`gradient_spinner`]: the 3×3 matrix with `cell_px` cells.
    Matrix { cell_px: f32 },
    /// [`mini_gradient_spinner`]: the 2×3 ring with `cell_px` cells.
    Mini { cell_px: f32 },
    /// [`status_arc`]: the running tool's rotating arc glyph.
    Arc { color: Hsla },
    /// [`pulse_wash`]: a rounded wash with a hairline border, its opacity
    /// pulsing (a loading thumbnail).
    Wash { bg: Hsla, border: Hsla, radius: f32 },
    /// [`progress_bar`]: the working trailer's progress bar, its glint and
    /// breathing ring moving once per [`PROGRESS_FLOW`] period.
    Progress(ProgressBar),
}

impl Loader {
    /// The repeating spec whose phase drives this loader.
    pub fn spec(&self) -> &'static MotionSpec {
        match self {
            Loader::Matrix { .. } | Loader::Mini { .. } => &GRADIENT_SPIN,
            Loader::Arc { .. } => &STATUS_SPIN,
            Loader::Wash { .. } => &CYPHER_PULSE,
            Loader::Progress(_) => &PROGRESS_FLOW,
        }
    }

    /// Paint this loader's frame at phase `delta` into `bounds`.
    pub fn paint(&self, bounds: Bounds<Pixels>, delta: f32, window: &mut Window, cx: &gpui::App) {
        match *self {
            Loader::Matrix { cell_px } => {
                for (offset, color) in matrix_cells(cell_px, delta) {
                    paint_cell(bounds.origin + offset, cell_px, color, window);
                }
            }
            Loader::Mini { cell_px } => paint_mini_spinner(bounds.origin, cell_px, delta, window),
            Loader::Arc { color } => {
                // `Transformation::rotate` about the glyph's center, built
                // here because the element-side conversion is private.
                let (center, scale) = (bounds.center(), window.scale_factor());
                let turn = TransformationMatrix::unit()
                    .translate(center.scale(scale))
                    .rotate(gpui::radians(delta * TAU))
                    .translate(center.scale(-scale));
                window
                    .paint_svg(bounds, ARC_GLYPH.into(), None, turn, color, cx)
                    .ok();
            }
            Loader::Progress(bar) => bar.paint(bounds, delta, window),
            Loader::Wash { bg, border, radius } => {
                let opacity = 0.35 + 0.4 * motion::pulse_wave(delta);
                window.paint_quad(gpui::quad(
                    bounds,
                    px(radius),
                    bg.opacity(opacity),
                    px(1.0),
                    border.opacity(opacity),
                    gpui::BorderStyle::Solid,
                ));
            }
        }
    }
}

/// The glyph [`Loader::Arc`] rotates.
const ARC_GLYPH: &str = crate::kit::icons::SPINNER;

/// A loader element (the caller sizes it): paints `loader`'s frame, leasing
/// the pulse clock for `view`, or records its slot inside a live layer.
fn loader_element(loader: Loader, view: EntityId) -> gpui::Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, cx| live_layer::paint_loader(loader, bounds, view, window, cx),
    )
}

fn paint_cell(origin: gpui::Point<Pixels>, cell_px: f32, color: Hsla, window: &mut Window) {
    let cell = Bounds::new(origin, gpui::size(px(cell_px), px(cell_px)));
    window.paint_quad(gpui::fill(cell, color).corner_radii(px(cell_px / 2.0)));
}

/// Side length of a run of `cells` cells `cell_px` wide, half a cell apart.
fn span(cells: usize, cell_px: f32) -> Pixels {
    px(cells as f32 * cell_px + (cells - 1) as f32 * cell_px / 2.0)
}

/// Every cell of the matrix at pulse `delta`: its offset from the grid's
/// top-left and its color. The per-cell phase follows the "arrow-up" pattern
/// (the pulse enters at the bottom edge and converges toward the top-center
/// cell), so the wave reads as travelling upward.
fn matrix_cells(cell_px: f32, delta: f32) -> impl Iterator<Item = (gpui::Point<Pixels>, Hsla)> {
    let center = (MATRIX_SIDE as f32 - 1.0) / 2.0;
    let max = MATRIX_SIDE as f32 - 1.0 + center;
    let pitch = cell_px * 1.5;
    (0..MATRIX_SIDE).flat_map(move |row| {
        let tint: Hsla = gpui::rgb(GSPIN_ROW_TINTS[row]).into();
        (0..MATRIX_SIDE).map(move |col| {
            // Distance of this cell from the wave origin, normalized into a
            // phase offset (gradient-spin's `--gspin-phase`).
            let d = MATRIX_SIDE as f32 - 1.0 - row as f32 + (col as f32 - center).abs();
            let phase = if max == 0.0 { 0.0 } else { d / (max + 1.0) };
            (
                gpui::point(px(col as f32 * pitch), px(row as f32 * pitch)),
                tint.opacity(motion::gspin_opacity(delta + phase, GSPIN_DIM)),
            )
        })
    })
}

/// The gradient matrix spinner (WorkingIndicator), ported from zeron's
/// gradient-spin.tsx: a 3×3 grid of round `cell_px` cells tinted per row
/// from the sunrise gradient, each pulsing opacity once per 750ms period in
/// a wave travelling upward. Outside a live layer it leases the pulse clock
/// for `view`.
pub fn gradient_spinner(cell_px: f32, view: EntityId) -> impl IntoElement {
    let side = span(MATRIX_SIDE, cell_px);
    loader_element(Loader::Matrix { cell_px }, view)
        .flex_none()
        .w(side)
        .h(side)
}

const MINI_COLS: usize = 2;
const MINI_ROWS: usize = 3;
/// Clockwise ring position of each `(row, col)` mini-spinner cell, top-left
/// first: (0,0) → (0,1) → (1,1) → (2,1) → (2,0) → (1,0).
const MINI_RING: [[usize; MINI_COLS]; MINI_ROWS] = [[0, 1], [5, 2], [4, 3]];

/// Every cell of a mini spinner at pulse `delta`: its offset from the grid's
/// top-left and its color (the row tint at the cell's pulse opacity).
fn mini_spinner_cells(
    cell_px: f32,
    delta: f32,
) -> impl Iterator<Item = (gpui::Point<Pixels>, Hsla)> {
    let appearance = crate::kit::theme::current_appearance();
    let tints = mini_row_tints(appearance);
    let dim = match appearance {
        Appearance::Dark => GSPIN_DIM,
        Appearance::Light => GSPIN_DIM_LIGHT,
    };
    let pitch = cell_px * 1.5;
    (0..MINI_ROWS).flat_map(move |row| {
        (0..MINI_COLS).map(move |col| {
            let phase = MINI_RING[row][col] as f32 / (MINI_COLS * MINI_ROWS) as f32;
            let opacity = motion::gspin_opacity(delta + phase, dim);
            (
                gpui::point(px(col as f32 * pitch), px(row as f32 * pitch)),
                tints[row].opacity(opacity),
            )
        })
    })
}

/// The footprint of a [`mini_gradient_spinner`] with `cell_px` cells.
pub fn mini_spinner_size(cell_px: f32) -> gpui::Size<Pixels> {
    gpui::size(span(MINI_COLS, cell_px), span(MINI_ROWS, cell_px))
}

/// Paint a [`mini_gradient_spinner`]'s frame at pulse `delta` straight into
/// the scene, its grid's top-left at `origin`.
pub fn paint_mini_spinner(
    origin: gpui::Point<Pixels>,
    cell_px: f32,
    delta: f32,
    window: &mut Window,
) {
    for (offset, color) in mini_spinner_cells(cell_px, delta) {
        paint_cell(origin + offset, cell_px, color, window);
    }
}

/// A 2×3 miniature of [`gradient_spinner`] sized for a status-dot slot
/// (sessions-sidebar working rows): same row tints and pulse timing, but the
/// brightness SNAKES around the grid's perimeter (every cell of a 2×3 grid is
/// on the ring) instead of sweeping as a vertical wave — a tiny radial chase.
/// ~6×10px footprint at the default 2.5px cells. Light mode deepens the tints
/// and rests the cells brighter ([`mini_row_tints`], [`GSPIN_DIM_LIGHT`]).
pub fn mini_gradient_spinner(cell_px: f32, view: EntityId) -> impl IntoElement {
    let size = mini_spinner_size(cell_px);
    loader_element(Loader::Mini { cell_px }, view)
        .flex_none()
        .w(size.width)
        .h(size.height)
}

/// The running tool's status glyph: the spinner arc in `color`, turning once
/// per [`STATUS_SPIN`] period; `size` square. Under reduced motion the arc
/// holds still.
pub fn status_arc(color: Hsla, size: f32, view: EntityId) -> impl IntoElement {
    loader_element(Loader::Arc { color }, view)
        .flex_none()
        .size(px(size))
}

/// A loading wash for a slot its parent sizes: `bg` filled, rounded
/// `radius`, with a 1px `border`, its opacity pulsing over [`CYPHER_PULSE`].
pub fn pulse_wash(bg: Hsla, border: Hsla, radius: f32, view: EntityId) -> impl IntoElement {
    loader_element(Loader::Wash { bg, border, radius }, view).size_full()
}

/// The working trailer's progress bar ([`ProgressBar`]), as wide as its
/// parent allows and [`crate::kit::progress_bar::HEIGHT`] tall.
pub fn progress_bar(bar: ProgressBar, view: EntityId) -> impl IntoElement {
    loader_element(Loader::Progress(bar), view)
        .w_full()
        .h(px(crate::kit::progress_bar::HEIGHT))
}

/// Full-window boot splash: a compact ASCII cypher wordmark over an opaque
/// background with an uppercase tracked "Loading" line.
/// While `fading` it plays `splash-out` (150ms hold, then 0.5s fade + 6px
/// lift); the shell removes it once [`SPLASH_OUT`] has run its course.
pub fn splash_overlay(theme: &Theme, fading: bool) -> AnyElement {
    let content = div()
        .absolute()
        .inset_0()
        // Loading uses a fully opaque backing; normal window/sidebar frost
        // remains unchanged once the splash has faded out.
        .bg(theme.glass().opacity(1.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(28.0))
        .child(loading_wordmark(theme))
        .child(loading_word(theme));
    if fading {
        motion::splash_out("boot-splash-out", content).into_any_element()
    } else {
        content.into_any_element()
    }
}

/// Keep rows left-aligned inside a centered block: centering each line would
/// break the glyph grid. Unlike the old comet, this compact mark needs no edge
/// fade, which would erase its ascender and descenders.
fn loading_wordmark(theme: &Theme) -> AnyElement {
    const FONT: f32 = 12.0;
    const LINE: f32 = 14.5;
    div()
        .flex()
        .flex_col()
        .items_start()
        // `.mono` preserves one advance per ASCII character.
        .mono(theme)
        .text_size(px(FONT))
        .line_height(px(LINE))
        // `theme.text` IS near-white on dark; on light it flips to the ink
        // tone rather than painting an invisible white block.
        .text_color(theme.text.opacity(0.7))
        .children(LOADING_WORDMARK.lines().map(|line| {
            div()
                .whitespace_nowrap()
                .child(SharedString::from(line.to_string()))
        }))
        .into_any_element()
}

/// Separate from the landing-page comet asset.
const LOADING_WORDMARK: &str = include_str!("../../assets/loading-wordmark.txt");

/// "L O A D I N G" — `text-[11px] uppercase tracking-[0.32em]
/// text-muted-foreground/70`; tracking approximated with thin spaces (gpui has
/// no letter-spacing at the pinned rev).
pub fn loading_word(theme: &Theme) -> impl IntoElement {
    div()
        .text_size(px(11.0))
        .text_color(theme.text_muted.opacity(0.7))
        .child(SharedString::from(
            "L\u{2009}O\u{2009}A\u{2009}D\u{2009}I\u{2009}N\u{2009}G",
        ))
}

// Compile-time proof the specs referenced here stay wired to the catalog.
const _: () = {
    assert!(SPLASH_OUT.delay_ms == 150);
    assert!(GRADIENT_SPIN.duration_ms == 750);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Every cell lands inside `size`, and the grid reaches its far edges.
    fn assert_fills(cells: &[(gpui::Point<Pixels>, Hsla)], cell: f32, size: gpui::Size<Pixels>) {
        for (offset, _) in cells {
            assert!(offset.x >= px(0.0) && offset.x + px(cell) <= size.width);
            assert!(offset.y >= px(0.0) && offset.y + px(cell) <= size.height);
        }
        let right = cells.iter().map(|(o, _)| o.x + px(cell)).max().unwrap();
        let bottom = cells.iter().map(|(o, _)| o.y + px(cell)).max().unwrap();
        assert_eq!((right, bottom), (size.width, size.height));
    }

    #[test]
    fn mini_spinner_cells_fill_their_footprint() {
        // The element is sized by `mini_spinner_size` and paints its cells
        // (or a live layer does) at these offsets.
        let cell = 2.0;
        let cells: Vec<_> = mini_spinner_cells(cell, 0.3).collect();
        assert_eq!(cells.len(), MINI_COLS * MINI_ROWS);
        assert_fills(&cells, cell, mini_spinner_size(cell));
    }

    #[test]
    fn matrix_cells_fill_their_footprint() {
        let cell = 2.5;
        let cells: Vec<_> = matrix_cells(cell, 0.3).collect();
        assert_eq!(cells.len(), MATRIX_SIDE * MATRIX_SIDE);
        let side = span(MATRIX_SIDE, cell);
        assert_fills(&cells, cell, gpui::size(side, side));
    }

    #[test]
    fn the_matrix_wave_rises_from_the_bottom_edge() {
        // At the cycle start the bottom-center cell is at full opacity and
        // the top-center one (the wave's far end) is still dim.
        let cells: Vec<_> = matrix_cells(2.0, 0.0).collect();
        let at = |row: usize, col: usize| cells[row * MATRIX_SIDE + col].1.a;
        assert_eq!(at(MATRIX_SIDE - 1, 1), 1.0);
        assert!(at(0, 1) < at(MATRIX_SIDE - 1, 1));
    }
}
