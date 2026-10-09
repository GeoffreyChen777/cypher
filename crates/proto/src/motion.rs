//! Loader motion — the pure math behind zeron's loading indicators.
//!
//! These are the curves and constants the gpui viewport animates with
//! (`cypher-ui/src/motion.rs`, `cypher-ui/src/loaders.rs`), lifted here so any
//! surface animates the *same* loaders rather than inventing its own spinner.
//! A loading indicator is a brand surface; two of them that disagree read as
//! two products.
//!
//! Everything is a pure function of a phase in `0..1`, so a caller can drive
//! it from a frame delta or from wall-clock elapsed time and get identical
//! output.

/// Side length of the gradient spinner matrix.
pub const MATRIX_SIDE: usize = 3;

/// Per-row tints of the gradient matrix spinner — zeron's "sunrise" gradient
/// sampled at each row: cool blue at the top, through amber, to pink.
pub const GSPIN_ROW_TINTS: [u32; MATRIX_SIDE] = [0xB6D3EF, 0xEDB185, 0xF888A0];
/// Opacity a gradient-spinner cell rests at between pulses.
pub const GSPIN_DIM: f32 = 0.1;

/// Linear interpolation.
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// A cell's phase, given the loader's raw phase and the cell's index.
pub fn staggered_phase(raw_delta: f32, index: usize, stagger: f32) -> f32 {
    (raw_delta - index as f32 * stagger).rem_euclid(1.0)
}

/// Cosine pulse: 0 at phase 0, 1 at phase 0.5, back to 0 at phase 1.
pub fn pulse_wave(phase: f32) -> f32 {
    0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
}

/// Gradient-spin cell opacity for a local phase `t` (0..1 of the period),
/// ported from zeron's `gradient-spin-pulse` keyframes: full at the cycle
/// start, easing down to `dim` by 45%, resting at `dim` until 92%, then rising
/// back to full — the per-cell phase offset sweeps this pulse across the grid.
pub fn gspin_opacity(t: f32, dim: f32) -> f32 {
    let t = t.rem_euclid(1.0);
    if t < 0.45 {
        lerp(1.0, dim, t / 0.45)
    } else if t < 0.92 {
        dim
    } else {
        lerp(dim, 1.0, (t - 0.92) / 0.08)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, what: &str) {
        assert!((a - b).abs() < 1e-5, "{what}: {a} vs {b}");
    }

    #[test]
    fn the_pulse_is_a_full_cosine_cycle() {
        close(pulse_wave(0.0), 0.0, "trough at 0");
        close(pulse_wave(0.5), 1.0, "crest at half");
        close(pulse_wave(1.0), 0.0, "trough at 1");
    }

    #[test]
    fn stagger_offsets_each_cell_and_wraps() {
        const STAGGER: f32 = 0.15 / 2.4;
        close(staggered_phase(0.0, 0, STAGGER), 0.0, "cell 0");
        close(
            staggered_phase(0.0, 1, STAGGER),
            1.0 - STAGGER,
            "cell 1 trails into the previous cycle",
        );
        // Phase is periodic: a whole extra turn changes nothing.
        close(
            staggered_phase(0.3, 2, STAGGER),
            staggered_phase(1.3, 2, STAGGER),
            "wraps",
        );
        // Always inside the unit interval, for any input.
        for raw in [-4.2f32, -0.1, 0.0, 0.5, 7.9] {
            for index in 0..5 {
                let phase = staggered_phase(raw, index, STAGGER);
                assert!((0.0..1.0).contains(&phase), "{raw} {index} -> {phase}");
            }
        }
    }

    #[test]
    fn gradient_spin_holds_dim_then_snaps_back() {
        close(gspin_opacity(0.0, GSPIN_DIM), 1.0, "starts full");
        close(gspin_opacity(0.45, GSPIN_DIM), GSPIN_DIM, "down by 45%");
        close(gspin_opacity(0.7, GSPIN_DIM), GSPIN_DIM, "rests dim");
        close(gspin_opacity(1.0, GSPIN_DIM), 1.0, "back to full");
        // Never leaves its bounds, at any phase.
        for step in 0..200 {
            let value = gspin_opacity(step as f32 / 100.0, GSPIN_DIM);
            assert!((GSPIN_DIM..=1.0).contains(&value), "{step} -> {value}");
        }
    }
}
