//! Loaders: the gradient matrix spinner and the boot splash content. All
//! motion routes through `crate::kit::motion` pure helpers, so the math is
//! unit-tested and these elements are testable-by-compile.
//!
//! Rendering pattern: every cell reads the one shared pulse clock
//! ([`motion::pulse_delta`]) and derives its own phase from it, so all cells
//! stay phase-locked. Cells animate inside fixed-size slots — opacity and
//! inner size are paint-local and never move surrounding layout.

use gpui::{AnyElement, App, EntityId, IntoElement, ParentElement, SharedString, Styled, div, px};

use crate::kit::motion::{self, GRADIENT_SPIN, SPLASH_OUT};
use crate::kit::theme::{MonoStyled, Theme};

// Shared with the terminal viewport (`cypher_proto::motion`) so both animate the
// same loaders from the same numbers.
pub use cypher_proto::motion::MATRIX_SIDE;

pub use cypher_proto::motion::{GSPIN_DIM, GSPIN_ROW_TINTS};

/// The gradient matrix spinner (WorkingIndicator), ported from zeron's
/// gradient-spin.tsx: a 3×3 grid of round cells tinted per row from the
/// sunrise gradient. Each cell pulses opacity once per 750ms period; the
/// per-cell phase follows the "arrow-up" pattern (the pulse enters at the
/// bottom edge and converges toward the top-center cell), so the wave reads
/// as travelling upward.
pub fn gradient_spinner(
    _id: &'static str,
    _theme: &Theme,
    cell_px: f32,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    let center = (MATRIX_SIDE as f32 - 1.0) / 2.0;
    let max = MATRIX_SIDE as f32 - 1.0 + center;
    let delta = motion::pulse_delta(&GRADIENT_SPIN, view, cx);
    div()
        .flex()
        .flex_col()
        .gap(px(cell_px / 2.0))
        .children((0..MATRIX_SIDE).map(move |row| {
            let tint: gpui::Hsla = gpui::rgb(GSPIN_ROW_TINTS[row]).into();
            div()
                .flex()
                .flex_row()
                .gap(px(cell_px / 2.0))
                .children((0..MATRIX_SIDE).map(move |col| {
                    // Distance of this cell from the wave origin, normalized
                    // into a phase offset (gradient-spin's `--gspin-phase`).
                    let d = MATRIX_SIDE as f32 - 1.0 - row as f32 + (col as f32 - center).abs();
                    let phase = if max == 0.0 { 0.0 } else { d / (max + 1.0) };
                    div()
                        .size(px(cell_px))
                        .rounded(px(cell_px / 2.0))
                        .bg(tint)
                        .opacity(motion::gspin_opacity(delta + phase, GSPIN_DIM))
                }))
        }))
}

/// A 2×3 miniature of [`gradient_spinner`] sized for a status-dot slot
/// (sessions-sidebar working rows): same row tints and pulse timing, but the
/// brightness SNAKES around the grid's perimeter (every cell of a 2×3 grid is
/// on the ring) instead of sweeping as a vertical wave — a tiny radial chase.
/// ~6×10px footprint at the default 2.5px cells.
pub fn mini_gradient_spinner(
    key: impl Into<SharedString>,
    cell_px: f32,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    const COLS: usize = 2;
    const ROWS: usize = 3;
    /// Clockwise ring position of each `(row, col)` cell, top-left first:
    /// (0,0) → (0,1) → (1,1) → (2,1) → (2,0) → (1,0).
    const RING: [[usize; COLS]; ROWS] = [[0, 1], [5, 2], [4, 3]];
    const RING_LEN: f32 = (COLS * ROWS) as f32;
    let _key = key.into();
    let delta = motion::pulse_delta(&GRADIENT_SPIN, view, cx);
    div()
        .flex()
        .flex_col()
        .gap(px(cell_px / 2.0))
        .children((0..ROWS).map(move |row| {
            let tint: gpui::Hsla = gpui::rgb(GSPIN_ROW_TINTS[row]).into();
            div()
                .flex()
                .flex_row()
                .gap(px(cell_px / 2.0))
                .children((0..COLS).map(move |col| {
                    let phase = RING[row][col] as f32 / RING_LEN;
                    div()
                        .size(px(cell_px))
                        .rounded(px(cell_px / 2.0))
                        .bg(tint)
                        .opacity(motion::gspin_opacity(delta + phase, GSPIN_DIM))
                }))
        }))
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
