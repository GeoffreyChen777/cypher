//! The terminal palette: the grid background and selection wash, the ANSI
//! tables tuned per [`Appearance`], and the 256-colour cube/grayscale
//! resolution. Pure colour functions over [`Theme`] so the appearance layer
//! can preview terminal colours without depending on the terminal view.

use gpui::Hsla;

use super::{Appearance, Theme, rgb_to_hsl};

/// Terminal background for an appearance.
///
/// Dark is `#191919`, one small step *up* from the app's `#161616` content
/// plane — the terminal reads as its own pane without becoming a lighter box.
/// Light mirrors that relationship rather than inverting the literal value:
/// the content plane is already pure white, so the terminal steps one notch
/// *down* to `#fafafa`. The step is bigger than dark's 3/255 for the reason the
/// theme's light surfaces are (`Theme::light`): a near-white delta that reads
/// as separation on near-black disappears entirely on white.
pub fn terminal_bg_for(appearance: Appearance) -> Hsla {
    match appearance {
        Appearance::Dark => rgb8(0x19, 0x19, 0x19),
        Appearance::Light => rgb8(0xfa, 0xfa, 0xfa),
    }
}

pub fn background(theme: &Theme) -> Hsla {
    theme
        .regions
        .terminal_background
        .unwrap_or_else(|| terminal_bg_for(theme.appearance))
}
pub fn selection(theme: &Theme) -> Hsla {
    theme
        .regions
        .terminal_selection
        .unwrap_or_else(|| terminal_selection_for(theme.appearance))
}

/// The panel fill behind the grid. On glass the opaque tone thins to a
/// translucent wash so the blurred desktop reads through like the rest of the
/// chrome (same move as [`Theme::card_glass_bg`]); opaque platforms keep the
/// true tone. Explicit cell backgrounds (vim colorschemes etc.) still paint
/// their own opaque quads on top.
pub fn terminal_panel_bg(theme: &Theme) -> Hsla {
    if let Some(background) = theme.regions.terminal_background {
        return background;
    }
    if theme.is_glass() {
        terminal_bg_for(theme.appearance).opacity(0.4)
    } else {
        terminal_bg_for(theme.appearance)
    }
}

/// Selection wash over the grid.
///
/// Deliberately *achromatic*, which is where this differs from
/// [`Theme::selection`] — the composer's blue. A saturated wash sits on top of
/// sixteen ANSI hues and drags every one of them toward itself: red text under
/// blue reads purple, green reads teal. A neutral veil changes lightness only,
/// so selected output keeps the colors the program asked for — the whole point
/// of tuning those palettes per appearance in the first place.
///
/// White on dark, black on light, the same direction [`crate::theme::ink`]
/// takes. The alpha is heavier than a hover wash because this has to read as a
/// deliberate highlight at a glance, and lighter than a plate because the
/// glyphs underneath still have to be legible through it.
pub fn terminal_selection_for(appearance: Appearance) -> Hsla {
    match appearance {
        Appearance::Dark => gpui::hsla(0.0, 0.0, 1.0, 0.22),
        // Slightly heavier: an equal alpha of black on near-white reads fainter
        // than white on near-black, because the surround is brighter to begin
        // with.
        Appearance::Light => gpui::hsla(0.0, 0.0, 0.0, 0.16),
    }
}

/// The 16 ANSI colors tuned for the near-black background (indexes 0-7 normal,
/// 8-15 bright).
const ANSI16_DARK: [(u8, u8, u8); 16] = [
    (0x32, 0x32, 0x32), // black — visible against #191919
    (0xf8, 0x71, 0x71), // red
    (0x4a, 0xde, 0x80), // green
    (0xfa, 0xcc, 0x15), // yellow
    (0x60, 0xa5, 0xfa), // blue
    (0xc0, 0x84, 0xfc), // magenta
    (0x22, 0xd3, 0xee), // cyan
    (0xd4, 0xd4, 0xd8), // white
    (0x52, 0x52, 0x5b), // bright black
    (0xfc, 0xa5, 0xa5), // bright red
    (0x86, 0xef, 0xac), // bright green
    (0xfd, 0xe0, 0x47), // bright yellow
    (0x93, 0xc5, 0xfd), // bright blue
    (0xd8, 0xb4, 0xfe), // bright magenta
    (0x67, 0xe8, 0xf9), // bright cyan
    (0xfa, 0xfa, 0xfa), // bright white
];

/// The same 16 slots for the light background — same hue families as
/// [`ANSI16_DARK`], moved down the tonal scale (the 400/300 steps the dark
/// table uses fail on white; these are the 600/700 steps, the same swap
/// `Theme::light` makes for its accents).
///
/// "Bright" stays *more prominent*, which on a light field means **darker**,
/// not lighter — a literal translation of the dark table would make the bright
/// half the invisible half, which is the bug this fixes in its purest form.
const ANSI16_LIGHT: [(u8, u8, u8); 16] = [
    (0x1f, 0x1f, 0x1f), // black
    (0xdc, 0x26, 0x26), // red — red-600
    (0x16, 0xa3, 0x4a), // green — green-600
    // Amber-700, not yellow-600: yellow is the one hue whose 600 step is still
    // bright enough to fail AA on white (2.8:1). `Theme::light` drops its
    // `warning` token to the same step for the same reason.
    (0xb4, 0x53, 0x09), // yellow — amber-700
    (0x25, 0x63, 0xeb), // blue — blue-600
    (0x93, 0x33, 0xea), // magenta — purple-600
    (0x0e, 0x74, 0x90), // cyan — cyan-700 (600 is too pale on white)
    (0x3f, 0x3f, 0x46), // white — the body-text tone, zinc-700
    (0x71, 0x71, 0x7a), // bright black — zinc-500
    (0xb9, 0x1c, 0x1c), // bright red — red-700
    (0x15, 0x80, 0x3d), // bright green — green-700
    (0x92, 0x40, 0x0e), // bright yellow — amber-800
    (0x1d, 0x4e, 0xd8), // bright blue — blue-700
    (0x7e, 0x22, 0xce), // bright magenta — purple-700
    (0x15, 0x5e, 0x75), // bright cyan — cyan-800
    (0x18, 0x18, 0x1b), // bright white — max emphasis, zinc-900
];

pub fn rgb8(r: u8, g: u8, b: u8) -> Hsla {
    let (h, s, l) = rgb_to_hsl(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    gpui::hsla(h, s, l, 1.0)
}

/// xterm 256-color cube component levels.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Resolve an indexed color (0-255) to RGB components for an appearance.
///
/// Three ranges, treated differently on purpose:
///
/// - **0-15** are *named* slots ("red", "bright blue"), not literal values —
///   every terminal emulator re-tints them per theme, so they swap tables.
/// - **16-231** is the 6×6×6 cube: a program asking for index 196 is asking for
///   `#ff0000` by arithmetic, and remapping it would be inventing colors the
///   caller did not pick. Left alone in both appearances, same as iTerm/Apple
///   Terminal light themes.
/// - **232-255** is the grayscale ramp, which tools use for *de-emphasis*
///   rather than for a specific grey. Its dark→light direction only reads as
///   "dim" on a dark background, so light mode mirrors the ramp: index 232
///   stays the faintest and 255 the strongest in both. Without this the ramp is
///   the single biggest legibility hole on white, because its bright end —
///   where most "dim hint" text lands — is the end that vanishes.
pub fn indexed_rgb(appearance: Appearance, index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => match appearance {
            Appearance::Dark => ANSI16_DARK[index as usize],
            Appearance::Light => ANSI16_LIGHT[index as usize],
        },
        16..=231 => {
            let n = index as usize - 16;
            (
                CUBE_LEVELS[n / 36],
                CUBE_LEVELS[(n / 6) % 6],
                CUBE_LEVELS[n % 6],
            )
        }
        232..=255 => {
            let step = index - 232;
            let step = match appearance {
                Appearance::Dark => step,
                Appearance::Light => 23 - step,
            };
            let v = 8 + 10 * step;
            (v, v, v)
        }
    }
}

/// An indexed (0-255) cell colour: an explicit region override for the 16
/// named slots, otherwise the appearance's table / cube / grey ramp.
pub fn ansi(theme: &Theme, index: u8) -> Hsla {
    if index < 16
        && let Some(color) = theme.regions.terminal_ansi[index as usize]
    {
        return color;
    }
    let (r, g, b) = indexed_rgb(theme.appearance, index);
    rgb8(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance, for contrast assertions (WCAG 2.x definition).
    fn luminance(r: u8, g: u8, b: u8) -> f32 {
        let f = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
        let (la, lb) = (luminance(a.0, a.1, a.2), luminance(b.0, b.1, b.2));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn cube_is_appearance_independent() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            // 16 = cube origin (0,0,0); 231 = cube max (255,255,255).
            assert_eq!(indexed_rgb(appearance, 16), (0, 0, 0));
            assert_eq!(indexed_rgb(appearance, 231), (255, 255, 255));
            // 196 = pure red corner: 16 + 36*5.
            assert_eq!(indexed_rgb(appearance, 196), (255, 0, 0));
            // 21 = pure blue corner.
            assert_eq!(indexed_rgb(appearance, 21), (0, 0, 255));
        }
    }

    #[test]
    fn grayscale_ramp_mirrors_in_light() {
        // Dark: 232 → 8 (faintest), 255 → 238 (strongest).
        assert_eq!(indexed_rgb(Appearance::Dark, 232), (8, 8, 8));
        assert_eq!(indexed_rgb(Appearance::Dark, 255), (238, 238, 238));
        // Light reverses it so the faint end stays faint against white.
        assert_eq!(indexed_rgb(Appearance::Light, 232), (238, 238, 238));
        assert_eq!(indexed_rgb(Appearance::Light, 255), (8, 8, 8));
    }

    #[test]
    fn ansi_range_hits_the_appearance_table() {
        assert_eq!(indexed_rgb(Appearance::Dark, 1), ANSI16_DARK[1]);
        assert_eq!(indexed_rgb(Appearance::Light, 1), ANSI16_LIGHT[1]);
        assert_ne!(ANSI16_DARK, ANSI16_LIGHT);
    }

    #[test]
    fn terminal_bg_tracks_the_appearance() {
        let dark = terminal_bg_for(Appearance::Dark);
        assert_eq!(dark.s, 0.0);
        assert!((dark.l - 25.0 / 255.0).abs() < 1e-4);

        let light = terminal_bg_for(Appearance::Light);
        assert_eq!(light.s, 0.0);
        assert!((light.l - 250.0 / 255.0).abs() < 1e-4);
    }

    /// Slot 0 is "black" — the one slot whose job is to sit *at* the dark end
    /// of the scale, not to be readable text. It draws box edges and shaded
    /// blocks, so it gets the grey floor below rather than the text floor.
    const ANSI_BLACK: usize = 0;

    /// The regression this module's light table exists to prevent: every ANSI
    /// slot has to be readable on its *own* background. The dark table on the
    /// light background is what the bug looked like.
    ///
    /// Two floors, because the table holds two kinds of thing. The twelve
    /// chromatic slots carry text and clear AA for the 13px mono the grid
    /// paints at. The black/bright-black pair are structural greys — box
    /// drawing and dim hints — and the *dark* palette, which shipped tuned by
    /// eye and is not what this change touches, puts bright-black at 2.58:1.
    /// Holding them to the text floor would fail the palette that was already
    /// correct, so they only have to separate from the background.
    #[test]
    fn every_ansi_slot_is_legible_on_its_background() {
        const MIN_TEXT: f32 = 3.0;
        const MIN_GREY: f32 = 1.25;
        let cases = [
            (Appearance::Dark, ANSI16_DARK, (0x09, 0x09, 0x09)),
            (Appearance::Light, ANSI16_LIGHT, (0xfa, 0xfa, 0xfa)),
        ];
        for (appearance, table, bg) in cases {
            for (ix, color) in table.iter().enumerate() {
                let grey = ix % 8 == ANSI_BLACK;
                let min = if grey { MIN_GREY } else { MIN_TEXT };
                let ratio = contrast(*color, bg);
                assert!(
                    ratio >= min,
                    "{appearance:?} ANSI {ix} {color:?} is {ratio:.2}:1 on {bg:?}, want {min}:1"
                );
            }
        }
    }

    /// "Bright" means *more* prominent in both appearances — which is darker on
    /// a light field, not lighter. A literal copy of the dark table would fail
    /// this in all seven checked slots.
    ///
    /// The black pair is the standing exception: "bright black" is the dim-text
    /// grey in every terminal palette, so it steps *lighter* than black in both
    /// appearances. Inverting it on light would put the most common dim-text
    /// color below `#1f1f1f`, i.e. heavier than body text — the opposite of dim.
    #[test]
    fn bright_slots_gain_emphasis_in_both_appearances() {
        let lum = |c: (u8, u8, u8)| luminance(c.0, c.1, c.2);
        for ix in 0..8 {
            if ix == ANSI_BLACK {
                continue;
            }
            assert!(
                lum(ANSI16_DARK[ix + 8]) > lum(ANSI16_DARK[ix]),
                "dark ANSI {ix}: bright should be lighter on near-black"
            );
            assert!(
                lum(ANSI16_LIGHT[ix + 8]) < lum(ANSI16_LIGHT[ix]),
                "light ANSI {ix}: bright should be darker on near-white"
            );
        }
        // The exception, asserted rather than merely skipped.
        assert!(lum(ANSI16_DARK[8]) > lum(ANSI16_DARK[ANSI_BLACK]));
        assert!(lum(ANSI16_LIGHT[8]) > lum(ANSI16_LIGHT[ANSI_BLACK]));
    }

    /// The selection veil must stay achromatic, or it tints the ANSI text it
    /// covers instead of just lifting it.
    #[test]
    fn selection_wash_is_neutral_and_translucent() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let wash = terminal_selection_for(appearance);
            assert_eq!(wash.s, 0.0, "{appearance:?} selection must have no hue");
            assert!(
                wash.a > 0.0 && wash.a < 0.5,
                "{appearance:?} selection must veil the cell, not replace it"
            );
        }
        // Opposite directions: lighten the dark grid, darken the light one.
        assert_eq!(terminal_selection_for(Appearance::Dark).l, 1.0);
        assert_eq!(terminal_selection_for(Appearance::Light).l, 0.0);
    }
}
