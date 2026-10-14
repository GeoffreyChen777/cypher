//! Text ripples: a still pool of characters behind an empty canvas. It shows
//! nothing at rest; a pointer moving across it drags a wake through the
//! water, and the rings spread, cross and settle back to nothing.
//!
//! [`Ripples`] is the pool: a height field on the character grid stepped by
//! the damped 2D wave equation at a fixed rate, independent of the frame
//! rate. Cells are taller than wide, so the two axes are weighted by their
//! own spacing and rings spread round in pixels, not in cells. [`field`]
//! paints the pool's current heights as monospace glyphs and feeds it the
//! pointer; it requests frames only while the water still moves.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Font, GlyphId, Hitbox, HitboxBehavior, Hsla, IntoElement,
    MouseMoveEvent, Pixels, SharedString, Styled, TextRun, Window, canvas, point, px,
};

use crate::kit::motion;
use crate::kit::theme::mono_font;

/// Glyph size, and the grid pitch (the width comes from the font's advance).
const FONT_SIZE: f32 = 12.0;
const CELL_HEIGHT: f32 = 15.0;
/// Simulation rate; frames in between paint the last step.
const STEP: Duration = Duration::from_micros(16_667);
/// Steps one frame may catch up on (a hitch never fast-forwards the pool).
const MAX_CATCH_UP: u32 = 4;
/// Squared wave travel per step, in cells (`(c·dt)²` over both axes): well
/// under the stability limit of 1, which keeps the rings smooth.
const COURANT: f32 = 0.3;
/// Energy kept per step; the rest settles out of the water.
const DAMPING: f32 = 0.982;
/// Cells along each edge that soak waves up instead of reflecting them back
/// into the canvas, and the extra energy their outermost cell takes a step.
const SPONGE: f32 = 6.0;
const SPONGE_LOSS: f32 = 0.14;
/// Heights below this are still water: the pool clears and stops ticking.
const REST: f32 = 0.012;
/// Height of one pointer touch, and its radius in pixels.
const TOUCH: f32 = 0.42;
const TOUCH_RADIUS: f32 = 11.0;
/// Pointer path spacing between touches, and the longest path one move
/// traces (a jump across the window is a new touch, not a long wake).
const TOUCH_SPACING: f32 = 5.0;
const MAX_STROKE: f32 = 240.0;
/// The largest grid the pool simulates; a huge window clips the rest.
const MAX_CELLS: usize = 48_000;

/// Crest glyphs (water raised), by rising height, and trough glyphs.
const CRESTS: [char; 4] = ['·', '-', '~', '≈'];
const TROUGHS: [char; 4] = ['.', '·', '·', ':'];
/// Heights below this paint nothing even while the pool moves.
const VISIBLE: f32 = 0.07;
/// A crest's opacity range; troughs paint at [`TROUGH_DIM`] of it.
const ALPHA_MIN: f32 = 0.04;
const ALPHA_MAX: f32 = 0.34;
const TROUGH_DIM: f32 = 0.7;

/// One cell's glyph: its index into [`CRESTS`] or [`TROUGHS`] and opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub crest: bool,
    pub level: usize,
    pub alpha: f32,
}

/// The water: a height field over `cols × rows` cells of `cell` pixels.
#[derive(Default)]
pub struct Ripples {
    cols: usize,
    rows: usize,
    cell: (f32, f32),
    cur: Vec<f32>,
    prev: Vec<f32>,
    /// Where the pointer last touched, in the field's pixels.
    pointer: Option<(f32, f32)>,
    /// The simulation clock while the water moves; `None` at rest.
    clock: Option<Instant>,
}

impl Ripples {
    /// Fit the pool to a `width × height` field of `cell`-sized cells. A new
    /// shape starts from still water.
    pub fn fit(&mut self, width: f32, height: f32, cell: (f32, f32)) {
        let mut cols = (width / cell.0).ceil().max(1.0) as usize;
        let rows = (height / cell.1).ceil().max(1.0) as usize;
        cols = cols.min(MAX_CELLS / rows.max(1)).max(1);
        if (cols, rows, cell) == (self.cols, self.rows, self.cell) {
            return;
        }
        *self = Self {
            cols,
            rows,
            cell,
            cur: vec![0.0; cols * rows],
            prev: vec![0.0; cols * rows],
            pointer: None,
            clock: None,
        };
    }

    /// Whether the water still moves (and needs frames).
    pub fn moving(&self) -> bool {
        self.clock.is_some()
    }

    /// The pointer moved to `at` over the water: trace a wake from where it
    /// last was.
    pub fn pointer_moved(&mut self, at: (f32, f32), now: Instant) {
        let from = self.pointer.replace(at).unwrap_or(at);
        let (dx, dy) = (at.0 - from.0, at.1 - from.1);
        let len = dx.hypot(dy);
        if len > MAX_STROKE {
            self.touch(at);
        } else {
            // Touches every few pixels, not per event: a wake's strength
            // follows the distance traveled, however often events arrive.
            let n = (len / TOUCH_SPACING).ceil().max(1.0) as usize;
            for k in 1..=n {
                let t = k as f32 / n as f32;
                self.touch((from.0 + dx * t, from.1 + dy * t));
            }
        }
        self.clock.get_or_insert(now);
    }

    /// The pointer left the water: its next touch starts a new wake.
    pub fn pointer_left(&mut self) {
        self.pointer = None;
    }

    /// Press the water down around `at` (pixels) with a soft round touch.
    fn touch(&mut self, at: (f32, f32)) {
        let (cw, ch) = self.cell;
        if self.cur.is_empty() || cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let (cx, cy) = (at.0 / cw - 0.5, at.1 / ch - 0.5);
        let (rx, ry) = (TOUCH_RADIUS / cw, TOUCH_RADIUS / ch);
        let x0 = (cx - rx).floor().max(0.0) as usize;
        let y0 = (cy - ry).floor().max(0.0) as usize;
        let x1 = ((cx + rx).ceil() as usize).min(self.cols - 1);
        let y1 = ((cy + ry).ceil() as usize).min(self.rows - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 - cx) * cw).hypot((y as f32 - cy) * ch) / TOUCH_RADIUS;
                if d < 1.0 {
                    // Raised-cosine falloff: no hard rim to ring off of.
                    let fall = 0.5 * (1.0 + (d * std::f32::consts::PI).cos());
                    self.cur[y * self.cols + x] -= TOUCH * fall;
                }
            }
        }
    }

    /// Run the steps due by `now`; the pool clears and stops once still.
    pub fn advance(&mut self, now: Instant) {
        let Some(clock) = self.clock else {
            return;
        };
        let due = (now.saturating_duration_since(clock).as_secs_f32() / STEP.as_secs_f32()) as u32;
        let steps = due.min(MAX_CATCH_UP);
        let mut peak = f32::MAX;
        for _ in 0..steps {
            peak = self.step();
        }
        self.clock = Some(if due > MAX_CATCH_UP {
            now
        } else {
            clock + STEP * due
        });
        if peak < REST {
            self.cur.fill(0.0);
            self.prev.fill(0.0);
            self.clock = None;
        }
    }

    /// One wave-equation step; returns the highest remaining |height|.
    fn step(&mut self) -> f32 {
        let (cols, rows) = (self.cols, self.rows);
        let (cw, ch) = self.cell;
        // `(c·dt)²` in pixels so both axes sum to COURANT cells per step.
        let travel = COURANT / (1.0 / (cw * cw) + 1.0 / (ch * ch));
        let (ax, ay) = (travel / (cw * cw), travel / (ch * ch));
        let mut peak = 0.0f32;
        // Leapfrog in place: the step's result overwrites `prev`, which only
        // its own cell reads. Edges hold still, behind the sponge.
        for y in 1..rows.saturating_sub(1) {
            let edge_y = y.min(rows - 1 - y) as f32;
            for x in 1..cols.saturating_sub(1) {
                let edge = edge_y.min(x.min(cols - 1 - x) as f32);
                let soak = (1.0 - edge / SPONGE).max(0.0);
                let keep = DAMPING * (1.0 - SPONGE_LOSS * soak * soak);
                let i = y * cols + x;
                let c = self.cur[i];
                let lap_x = self.cur[i - 1] + self.cur[i + 1] - 2.0 * c;
                let lap_y = self.cur[i - cols] + self.cur[i + cols] - 2.0 * c;
                let next = (2.0 * c - self.prev[i] + ax * lap_x + ay * lap_y) * keep;
                self.prev[i] = next;
                peak = peak.max(next.abs());
            }
        }
        std::mem::swap(&mut self.cur, &mut self.prev);
        peak
    }

    /// The glyph cell `(x, y)` shows, if any.
    pub fn glyph(&self, x: usize, y: usize) -> Option<Glyph> {
        glyph_for(*self.cur.get(y * self.cols + x)?)
    }
}

/// The glyph for a water height: rising crests climb [`CRESTS`], deepening
/// troughs [`TROUGHS`], both brightening with the height.
pub fn glyph_for(h: f32) -> Option<Glyph> {
    let i = h.abs().min(1.0);
    if i < VISIBLE {
        return None;
    }
    let level = ((i - VISIBLE) / (1.0 - VISIBLE) * CRESTS.len() as f32) as usize;
    let crest = h > 0.0;
    let alpha = ALPHA_MIN + (ALPHA_MAX - ALPHA_MIN) * i.sqrt();
    Some(Glyph {
        crest,
        level: level.min(CRESTS.len() - 1),
        alpha: if crest { alpha } else { alpha * TROUGH_DIM },
    })
}

/// A shaped glyph ready to stamp into any cell.
#[derive(Clone, Copy)]
struct Stamp {
    font: gpui::FontId,
    glyph: GlyphId,
    /// Offset from the cell's left edge that centers the glyph.
    dx: Pixels,
}

/// The ripple field over its parent (absolute, full size): paints `pool`
/// in `color`'s monospace `family` and stirs it with the pointer. The
/// bottom `inert` pixels (the composer floating there) paint ripples but
/// never start them. Reduced motion keeps the water still.
pub fn field(
    pool: Rc<RefCell<Ripples>>,
    color: Hsla,
    family: SharedString,
    inert: f32,
) -> AnyElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, cx| {
            paint(&pool, color, &family, inert, bounds, hitbox, window, cx)
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn paint(
    pool: &Rc<RefCell<Ripples>>,
    color: Hsla,
    family: &SharedString,
    inert: f32,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    if motion::reduced_motion(cx) {
        *pool.borrow_mut() = Ripples::default();
        return;
    }
    let font = mono_font(family.clone());
    let stamps = shape(&font, &CRESTS, window).zip(shape(&font, &TROUGHS, window));
    let Some(((crests, cell_w, baseline), (troughs, _, _))) = stamps else {
        return;
    };
    let origin = bounds.origin;
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    {
        let mut water = pool.borrow_mut();
        water.fit(width, height, (cell_w, CELL_HEIGHT));
        water.advance(Instant::now());
        if water.moving() {
            window.request_animation_frame();
            for y in 0..water.rows {
                for x in 0..water.cols {
                    let Some(glyph) = water.glyph(x, y) else {
                        continue;
                    };
                    let stamp = if glyph.crest { crests } else { troughs }[glyph.level];
                    let at = point(
                        origin.x + px(x as f32 * cell_w) + stamp.dx,
                        origin.y + px(y as f32 * CELL_HEIGHT) + baseline,
                    );
                    let tint = color.opacity(glyph.alpha);
                    let _ = window.paint_glyph(at, stamp.font, stamp.glyph, px(FONT_SIZE), tint);
                }
            }
        }
    }
    let pool = pool.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
        if !phase.bubble() {
            return;
        }
        let local = event.position - origin;
        let (x, y) = (f32::from(local.x), f32::from(local.y));
        let mut water = pool.borrow_mut();
        // Over the water and nothing occluding it (menus, dialogs).
        if hitbox.is_hovered(window) && y < height - inert {
            let was_still = !water.moving();
            water.pointer_moved((x, y), Instant::now());
            if was_still {
                window.refresh();
            }
        } else {
            water.pointer_left();
        }
    });
}

/// Shape `chars` in `font`: one stamp each, plus the cell width (the
/// font's advance) and the baseline's offset into a cell.
fn shape<const N: usize>(
    font: &Font,
    chars: &[char; N],
    window: &mut Window,
) -> Option<([Stamp; N], f32, Pixels)> {
    let size = px(FONT_SIZE);
    let line = |text: SharedString, window: &mut Window| {
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(text, size, &[run], None)
    };
    let pitch = line("0".into(), window);
    let cell_w = f32::from(pitch.width);
    let baseline = px((CELL_HEIGHT - f32::from(pitch.ascent + pitch.descent)) / 2.0) + pitch.ascent;
    let mut stamps = Vec::with_capacity(N);
    for ch in chars {
        let shaped = line(ch.to_string().into(), window);
        let run = shaped.runs.first()?;
        let glyph = run.glyphs.first()?;
        stamps.push(Stamp {
            font: run.font_id,
            glyph: glyph.id,
            dx: (px(cell_w) - shaped.width) / 2.0,
        });
    }
    Some((stamps.try_into().ok()?, cell_w, baseline))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> Ripples {
        let mut water = Ripples::default();
        water.fit(480.0, 300.0, (7.2, CELL_HEIGHT));
        water
    }

    fn run(water: &mut Ripples, start: Instant, ms: u64) -> Instant {
        let end = start + Duration::from_millis(ms);
        let mut t = start;
        while t < end {
            t += Duration::from_millis(16);
            water.advance(t);
        }
        t
    }

    fn peak(water: &Ripples) -> f32 {
        water.cur.iter().fold(0.0f32, |m, h| m.max(h.abs()))
    }

    #[test]
    fn still_water_paints_nothing_and_needs_no_frames() {
        let water = pool();
        assert!(!water.moving());
        assert!((0..water.rows).all(|y| (0..water.cols).all(|x| water.glyph(x, y).is_none())));
    }

    #[test]
    fn a_wake_spreads_then_settles_to_rest() {
        let mut water = pool();
        let t0 = Instant::now();
        water.pointer_moved((100.0, 150.0), t0);
        water.pointer_moved((260.0, 150.0), t0);
        assert!(water.moving());
        let t = run(&mut water, t0, 300);
        // The rings reached cells well away from the path (row 10).
        let far =
            (0..water.cols).any(|x| water.glyph(x, 6).is_some() || water.glyph(x, 14).is_some());
        assert!(far, "rings spread off the wake");
        assert!(peak(&water) <= 1.5, "stable: {}", peak(&water));
        run(&mut water, t, 8_000);
        assert!(!water.moving(), "settled");
        assert_eq!(peak(&water), 0.0);
    }

    #[test]
    fn wake_strength_follows_distance_not_event_count() {
        let t0 = Instant::now();
        let mut once = pool();
        once.pointer_moved((100.0, 150.0), t0);
        once.pointer_moved((200.0, 150.0), t0);
        let mut often = pool();
        for k in 0..=20 {
            often.pointer_moved((100.0 + k as f32 * 5.0, 150.0), t0);
        }
        let sum = |w: &Ripples| w.cur.iter().sum::<f32>();
        assert!((sum(&once) - sum(&often)).abs() < 0.05 * sum(&once).abs());
    }

    #[test]
    fn a_hitch_never_fast_forwards() {
        let mut water = pool();
        let t0 = Instant::now();
        water.pointer_moved((100.0, 150.0), t0);
        water.advance(t0 + Duration::from_secs(5));
        assert!(water.moving(), "only MAX_CATCH_UP steps ran");
    }

    #[test]
    fn glyphs_climb_with_height() {
        assert_eq!(glyph_for(0.01), None);
        let low = glyph_for(0.1).unwrap();
        let high = glyph_for(0.9).unwrap();
        assert!(low.crest && high.crest);
        assert!(high.level > low.level && high.alpha > low.alpha);
        let trough = glyph_for(-0.9).unwrap();
        assert!(!trough.crest && trough.alpha < high.alpha);
    }

    /// `cargo test -p cypher-ui ripples::tests::preview -- --ignored --nocapture`
    /// prints a few frames of a wake, for tuning by eye.
    #[test]
    #[ignore]
    fn preview() {
        let mut water = Ripples::default();
        water.fit(720.0, 330.0, (7.2, CELL_HEIGHT));
        let t0 = Instant::now();
        let mut t = t0;
        for k in 0..=30 {
            t += Duration::from_millis(16);
            water.pointer_moved((150.0 + k as f32 * 12.0, 120.0 + k as f32 * 2.0), t);
            water.advance(t);
        }
        for ms in [0, 400, 800, 800] {
            t = run(&mut water, t, ms);
            println!("--- +{ms}ms");
            for y in 0..water.rows {
                let row: String = (0..water.cols)
                    .map(|x| match water.glyph(x, y) {
                        None => ' ',
                        Some(g) if g.crest => CRESTS[g.level],
                        Some(g) => TROUGHS[g.level],
                    })
                    .collect();
                println!("{row}");
            }
        }
    }
}
