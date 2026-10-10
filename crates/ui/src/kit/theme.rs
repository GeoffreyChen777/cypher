//! The app theme — two concrete appearances, one token set.
//!
//! Colors are precomputed from an oklch-derived neutral scale (perceptually even
//! lightness steps; the same scale zeron's Tailwind theme used) into gpui [`Hsla`].
//! **Numbers drive layout, colors are paint**: layout constants live here as plain
//! numbers and never depend on which color is painted.
//!
//! # Light is designed, not inverted
//!
//! Mirroring lightness produces the classic "washed-out inverted" look, for three
//! reasons this module handles explicitly:
//!
//! 1. **Surface order flips meaning.** In dark, the main content panel is the
//!    *darkest* plane and raised surfaces get *lighter*. In light, the content
//!    panel is *white* and the shell/sidebar goes *grey* — chrome recedes by
//!    getting darker, not lighter. Popovers stay white and earn separation from a
//!    border and shadow rather than from lightness.
//! 2. **Elevation reverses.** On dark, a faint *white* wash means "raised". Its
//!    literal translation — a faint *black* wash on white — means "recessed", so
//!    the composer read as a dent instead of a plate. Light lifts with white plus
//!    a border and shadow ([`Theme::input_bg`], the elevation ladder). Fill
//!    *alphas* carry over unchanged ([`INK_FILL_SCALE`]); only hairlines scale, so
//!    a 1px edge survives a bright surround ([`INK_HAIRLINE_SCALE`]).
//! 3. **Accents must move down the scale.** The dark palette's 400-level accents
//!    (indigo/red/amber) are chosen for contrast against near-black; on white they
//!    fall to 2–4:1 and fail WCAG AA. Light mode uses the 600-level siblings at the
//!    same hue, which restores the *contrast ratio* the dark token had.
//!
//! Text tones are chosen so each light token lands within ~0.5 of its dark
//! counterpart's contrast ratio against its own background — the pairing is
//! verified in [`tests::text_contrast_is_paired_across_appearances`], not eyeballed.
//!
//! Installed as a gpui [`Global`] at boot; read with [`Theme::of`].

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use cypher_syntax::HighlightKind;
use gpui::{App, Global, Hsla, SharedString, hsla};

pub mod terminal;

/// Which appearance the app is painting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

impl Appearance {
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    /// Map a gpui window appearance onto ours (both vibrant variants are just
    /// the blurred flavour of the same tone).
    pub fn from_window(appearance: gpui::WindowAppearance) -> Self {
        use gpui::WindowAppearance::*;
        match appearance {
            Light | VibrantLight => Self::Light,
            Dark | VibrantDark => Self::Dark,
        }
    }
}

/// Process-wide mirror of the installed theme's appearance.
///
/// The paint helpers ([`ink`], [`hairline`], [`wash`], …) are free functions
/// called from deep inside element builders that have no `cx` in scope, so they
/// read the appearance from here instead of the gpui global. Appearance is
/// genuinely process-wide — one setting for every window — so a single mirror is
/// sound; the appearance layer's `install_theme` is the only writer outside
/// tests.
static CURRENT_APPEARANCE: AtomicU8 = AtomicU8::new(0);

/// Bumped every time the appearance actually changes.
///
/// Anything that caches *resolved colors* — most importantly the markdown
/// renderer's cross-frame `TextRun` cache, which bakes an `Hsla` into every run —
/// is only valid for the palette that produced it. Those caches were written when
/// the theme was a compile-time constant, so their validity keys cover content
/// only. Rather than thread the palette through every key, they compare this
/// counter and drop everything when it moves.
static THEME_GENERATION: AtomicU32 = AtomicU32::new(0);

/// The appearance the context-free paint helpers are painting for.
pub fn current_appearance() -> Appearance {
    match CURRENT_APPEARANCE.load(Ordering::Relaxed) {
        1 => Appearance::Light,
        _ => Appearance::Dark,
    }
}

/// Monotonic id of the current palette — see [`THEME_GENERATION`].
pub fn theme_generation() -> u32 {
    THEME_GENERATION.load(Ordering::Relaxed)
}

/// [`CURRENT_APPEARANCE`] is process-wide, so under the parallel test runner
/// any test that flips it — or asserts on the output of a helper that reads it
/// ([`ink`], [`hairline`], [`wash`], …) — must hold this lock. Crate-visible
/// because such tests exist outside this module too (see `motion::tests`).
/// Tests that flip the appearance restore Dark before releasing the guard.
#[cfg(test)]
pub fn lock_appearance() -> std::sync::MutexGuard<'static, ()> {
    static APPEARANCE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    super::lock(&APPEARANCE_LOCK)
}

/// Point the context-free paint helpers at an appearance. Called by the
/// appearance layer's `install_theme`; exposed for tests that build a theme
/// without an `App`.
pub fn set_current_appearance(appearance: Appearance) {
    let encoded = match appearance {
        Appearance::Dark => 0,
        Appearance::Light => 1,
    };
    if CURRENT_APPEARANCE.swap(encoded, Ordering::Relaxed) != encoded {
        THEME_GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Light-mode alpha multiplier for **fills** (hover/active washes, chip and pill
/// backgrounds).
///
/// This was 0.5 on the theory that dark ink on a bright field reads heavier and
/// should be scaled back. That theory is right for a *large* wash and badly wrong
/// for everything else: this palette leans on very low alphas for its subtle
/// fills — the composer plate is `ink(0.03)`, key caps are `ink(0.05)` — and
/// halving those produced 1.5% black on white, which is nothing. The composer
/// lost its background entirely and selected tabs stopped reading as selected.
///
/// The established light-UI scales (Primer, Radix) land subtle ≈ 3–4%, hover ≈ 8%,
/// selected ≈ 14% black — which is where the dark palette's white alphas already
/// sit. So the honest multiplier is 1: the same number in both appearances, with
/// only the *tone* flipping. Any per-state correction belongs in that state's
/// token, not in a blanket multiplier.
pub const INK_FILL_SCALE: f32 = 1.0;

/// Light-mode alpha multiplier for **hairlines** (borders, dividers, rings).
/// Opposite of fills: a 1px edge has to hold its own against a bright surround,
/// and the dark palette's white hairlines are deliberately faint. Scaling up
/// keeps separators legible instead of dissolving into the panel.
pub const INK_HAIRLINE_SCALE: f32 = 1.35;

/// Paint-only syntax colors. The hues follow the Git history graph's lane
/// palette (indigo, pink, emerald, amber, red, neutral), while light-mode
/// variants are darkened enough to remain readable as text on white.
#[derive(Debug, Clone)]
pub struct SyntaxPalette {
    pub comment: Hsla,
    pub keyword: Hsla,
    pub string: Hsla,
    pub string_special: Hsla,
    pub escape: Hsla,
    pub number: Hsla,
    pub boolean: Hsla,
    pub type_name: Hsla,
    pub type_builtin: Hsla,
    pub constructor: Hsla,
    pub function: Hsla,
    pub function_builtin: Hsla,
    pub macro_name: Hsla,
    pub property: Hsla,
    pub constant: Hsla,
    pub variable: Hsla,
    pub variable_special: Hsla,
    pub parameter: Hsla,
    pub operator: Hsla,
    pub punctuation: Hsla,
    pub tag: Hsla,
    pub attribute: Hsla,
    pub label: Hsla,
    pub invalid: Hsla,
}

impl SyntaxPalette {
    pub fn color(&self, kind: HighlightKind) -> Hsla {
        match kind {
            HighlightKind::Comment => self.comment,
            HighlightKind::Keyword => self.keyword,
            HighlightKind::String => self.string,
            HighlightKind::StringSpecial => self.string_special,
            HighlightKind::Escape => self.escape,
            HighlightKind::Number => self.number,
            HighlightKind::Boolean => self.boolean,
            HighlightKind::Type => self.type_name,
            HighlightKind::TypeBuiltin => self.type_builtin,
            HighlightKind::Constructor => self.constructor,
            HighlightKind::Function => self.function,
            HighlightKind::FunctionBuiltin => self.function_builtin,
            HighlightKind::Macro => self.macro_name,
            HighlightKind::Property => self.property,
            HighlightKind::Constant => self.constant,
            HighlightKind::Variable => self.variable,
            HighlightKind::VariableSpecial => self.variable_special,
            HighlightKind::Parameter => self.parameter,
            HighlightKind::Operator => self.operator,
            HighlightKind::Punctuation | HighlightKind::Embedded => self.punctuation,
            HighlightKind::Tag => self.tag,
            HighlightKind::Attribute => self.attribute,
            HighlightKind::Label => self.label,
            HighlightKind::Invalid => self.invalid,
        }
    }

    fn dark(text: Hsla, comment: Hsla, danger: Hsla) -> Self {
        // Same sources and 72% saturation treatment as history::graph_color.
        let indigo = git_graph_tone(oklch(0.673, 0.182, 276.935));
        let pink = git_graph_tone(oklch(0.718, 0.202, 349.761));
        let emerald = git_graph_tone(oklch(0.765, 0.177, 163.223));
        let amber = git_graph_tone(oklch(0.828, 0.189, 84.429));
        let red = git_graph_tone(danger);
        Self {
            comment,
            keyword: indigo,
            string: emerald,
            string_special: pink,
            escape: pink,
            number: amber,
            boolean: amber,
            type_name: amber,
            type_builtin: emerald,
            constructor: amber,
            function: indigo,
            function_builtin: pink,
            macro_name: pink,
            property: amber,
            constant: emerald,
            variable: text,
            variable_special: pink,
            parameter: text,
            operator: text,
            punctuation: text,
            tag: pink,
            attribute: amber,
            label: amber,
            invalid: red,
        }
    }

    fn light(text: Hsla, comment: Hsla, danger: Hsla) -> Self {
        // Match the light graph's hue families at text-safe lightness.
        let indigo = git_graph_tone(oklch(0.47, 0.20, 276.966));
        let pink = git_graph_tone(oklch(0.47, 0.17, 0.584));
        let emerald = git_graph_tone(oklch(0.46, 0.11, 163.225));
        let amber = git_graph_tone(oklch(0.47, 0.12, 48.998));
        let red = git_graph_tone(danger);
        Self {
            comment,
            keyword: indigo,
            string: emerald,
            string_special: pink,
            escape: pink,
            number: amber,
            boolean: amber,
            type_name: amber,
            type_builtin: emerald,
            constructor: amber,
            function: indigo,
            function_builtin: pink,
            macro_name: pink,
            property: amber,
            constant: emerald,
            variable: text,
            variable_special: pink,
            parameter: text,
            operator: text,
            punctuation: text,
            tag: pink,
            attribute: amber,
            label: amber,
            invalid: red,
        }
    }
}

/// Git history intentionally softens lane saturation so the graph remains
/// colorful without competing with content. Syntax uses the same treatment.
fn git_graph_tone(mut color: Hsla) -> Hsla {
    color.s *= 0.72;
    color
}

/// Default markdown metrics, overridden only by a chat's scoped theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkdownMetrics {
    pub body_size: f32,
    pub body_line_height: f32,
    pub code_size: f32,
    pub code_line_height: f32,
    pub block_gap: f32,
}

impl Default for MarkdownMetrics {
    fn default() -> Self {
        Self {
            body_size: 14.0,
            body_line_height: 22.0,
            code_size: 12.5,
            code_line_height: 18.0,
            block_gap: 12.0,
        }
    }
}

/// Runtime-only tokens. Region overrides are applied to cloned Themes, never
/// to the global theme or to another region's renderers.
#[derive(Clone, Debug, Default)]
pub struct RegionTokens {
    pub chat_background: Option<Hsla>,
    pub terminal_background: Option<Hsla>,
    pub terminal_selection: Option<Hsla>,
    pub terminal_ansi: [Option<Hsla>; 16],
    pub git_background: Option<Hsla>,
    pub git_line_number: Option<Hsla>,
    pub git_added: Option<Hsla>,
    pub git_deleted: Option<Hsla>,
    pub sidebar_selected: Option<Hsla>,
    pub sidebar_hover: Option<Hsla>,
}

/// The app theme. Two concrete instances — [`Theme::dark`] and [`Theme::light`].
#[derive(Debug, Clone)]
pub struct Theme {
    pub regions: RegionTokens,
    pub markdown: MarkdownMetrics,
    /// Distinguishes cached chat runs after typography/custom-color changes.
    pub text_style_revision: u64,
    /// None preserves the original monochrome links and contextual bubble wash.
    pub markdown_link: Option<Hsla>,
    pub user_bubble: Option<Hsla>,
    /// Fenced-code overrides, applied only by the chat Markdown renderer.
    pub code_block_background: Option<Hsla>,
    pub code_block_text: Option<Hsla>,
    /// Markdown inline-code overrides, separate from the mention-chip tokens.
    pub inline_code_text: Option<Hsla>,
    pub inline_code_background: Option<Hsla>,
    /// Which appearance these tokens were built for.
    pub appearance: Appearance,

    // ---- paint: neutral surfaces ----
    /// Main content panel. Dark: the deepest plane (#161616). Light: pure white —
    /// long-form content reads best on an unbroken white field.
    pub bg: Hsla,
    /// Shell / sidebar surface. Dark: one step *up* from `bg`. Light: one step
    /// *down* (grey) — chrome recedes from the content plane in both, which is
    /// the direction a naive invert gets backwards.
    pub surface: Hsla,
    /// Raised surface: opaque pills and chips that sit proud of the panel.
    /// Dark: lighter than `surface`. Light: white, separated by `border` +
    /// shadow rather than by lightness.
    pub surface_raised: Hsla,

    // ---- paint: elevation ladder ----
    //
    // Dark mode distinguishes floating planes by lightness, and the steps are
    // *small* (#0e → #10 → #16 → #1e). They are not interchangeable: collapsing
    // them onto one token visibly lifts popovers off their intended plane.
    //
    // Light mode cannot use the same trick, because the content plane is already
    // white and there is nothing lighter to climb to. All three land on white and
    // let `border` + shadow carry the separation instead — the standard light-UI
    // answer, and the reason this is a ladder of tokens rather than an arithmetic
    // offset applied to one.
    /// Inline card resting on the main panel (auth gate, empty-state cards).
    pub surface_card: Hsla,
    /// Modal dialog, floating over a [`Theme::scrim`].
    pub surface_dialog: Hsla,
    /// Popover, menu and command-palette surface — the highest plane.
    pub surface_overlay: Hsla,
    /// Hover wash for interactive rows/buttons.
    pub element_hover: Hsla,
    /// Active/selected wash.
    pub element_active: Hsla,
    /// Hairline border.
    pub border: Hsla,
    /// Stronger border for focused/raised edges.
    pub border_strong: Hsla,

    // ---- paint: text ----
    /// Primary text. ~17.5:1 on its own background in both appearances.
    pub text: Hsla,
    /// Muted text: timestamps, secondary labels. ~7.5–8:1.
    pub text_muted: Hsla,
    /// Faint text: placeholders, disabled. ~4.5:1 — AA for body copy.
    pub text_faint: Hsla,
    /// One notch below `text_muted` — the diff file-path tone. It exists as its
    /// own token rather than being folded into `text_muted` because the dark
    /// value was sampled (#989898) and folding it would shift that label, which
    /// is a palette change dressed up as a refactor.
    pub text_dim: Hsla,

    // ---- paint: high-contrast solid (primary buttons) ----
    /// The maximum-contrast solid fill: near-white on dark, near-black on light.
    /// This is the primary button plate.
    pub solid: Hsla,
    /// Label/icon color on top of [`Self::solid`] — its inverse.
    pub on_solid: Hsla,

    // ---- paint: accents ----
    /// Accent — indigo. Text/icon weight: indigo-400 on dark, indigo-600 on light
    /// (the 400 fails AA on white).
    pub accent: Hsla,
    /// Stronger accent for filled accent plates.
    pub accent_strong: Hsla,
    /// Danger — red (errors, stop button).
    pub danger: Hsla,
    /// Softer danger for secondary/inline error copy.
    pub danger_muted: Hsla,
    /// Warning — amber (offline notices, awaiting-input).
    pub warning: Hsla,
    /// Softer warning for secondary copy.
    pub warning_muted: Hsla,
    /// Success / online — emerald.
    pub success: Hsla,
    /// Working / streaming indicator — pink.
    pub busy: Hsla,
    /// Softer success for text on a success-tinted chip.
    pub success_muted: Hsla,

    // ---- paint: components ----
    /// Hover tone for an *opaque* raised pill. Hover must brighten the plate in
    /// dark mode, never swap it for a translucent wash (that made pills go
    /// see-through — user-reported); in light mode it darkens instead, same idea.
    pub surface_raised_hover: Hsla,
    /// The composer pill and other input plates.
    ///
    /// Its own token because "lifted" inverts between appearances. On dark, a
    /// faint *white* wash over near-black reads as raised. The literal light
    /// translation — a faint *black* wash on white — reads as **recessed**, a dent
    /// rather than a plate, which is why the prompt looked like bare text on a
    /// smudge. Light mode lifts the way light UIs actually do: pure white, with
    /// the border and shadow carrying the elevation.
    pub input_bg: Hsla,
    /// Text-selection highlight in the composer and inputs.
    pub selection: Hsla,
    /// Terminal block cursor.
    pub cursor: Hsla,
    /// Composer text caret. A blue distinct from `accent` — sampled from the
    /// original composer, not derived, so it keeps its own token.
    pub caret: Hsla,
    /// Destructive-action button fill (danger plate).
    pub danger_strong: Hsla,

    // ---- paint: code & diff ----
    /// Inline-code and mention-chip text — emerald.
    pub code_text: Hsla,
    /// Inline-code wash behind [`Self::code_text`].
    pub code_wash: Hsla,
    /// Shared paint-only syntax palette.
    pub syntax: SyntaxPalette,
    /// Diff: added lines.
    pub diff_add: Hsla,
    /// Diff: deleted lines.
    pub diff_del: Hsla,
    /// Diff: hunk-header wash (bluish grey).
    pub diff_hunk_bg: Hsla,

    // ---- fonts ----
    /// UI font family (the bundled Geist).
    pub font_sans: SharedString,
    /// Monospace family for code/terminal.
    pub font_mono: SharedString,
}

/// Font features for every monospace run: coding ligatures OFF.
///
/// Not a taste call — the bundled Geist Mono 1.700 ships its coding ligatures
/// as a plain many-to-one `liga` substitution (`= = =` → one
/// `equal_equal_equal.liga`), but the glyph is drawn for the *spacer*
/// construction: advance 600 (one cell) with ink from x=-1138 to x=538, i.e.
/// meant to sit in the LAST of three cells and paint backwards over the first
/// two. Upstream drives that from `ss11`, which first substitutes two blank
/// `SPC` glyphs to hold the cells; 1.700 also exposed the bare ligature through
/// default-on `liga`, so shaping `model.api === a` yields 13 glyphs instead of
/// 15: the three-bar glyph lands on the first `=`, overlaps the text to its
/// left, and the rest of the line slides two cells back. `calt`/`dlig` go off
/// with it because a user-chosen code font (Fira Code and friends) drives the
/// same ligatures from those features instead.
pub fn mono_features() -> gpui::FontFeatures {
    gpui::FontFeatures(std::sync::Arc::new(vec![
        ("liga".into(), 0),
        ("calt".into(), 0),
        ("dlig".into(), 0),
    ]))
}

/// A [`gpui::Font`] for the given monospace family with [`mono_features`]
/// applied — the only way code text should build a `TextRun` font.
pub fn mono_font(family: SharedString) -> gpui::Font {
    let mut font = gpui::font(family);
    font.features = mono_features();
    font
}

/// `.mono(theme)` on any styled element: the mono family plus ligatures off.
/// Inherited by child text, but a `TextRun` carrying its own [`gpui::Font`]
/// overrides it — those build through [`mono_font`].
pub trait MonoStyled: gpui::Styled + Sized {
    fn mono(self, theme: &Theme) -> Self {
        self.font_family(theme.font_mono.clone())
            .font_features(mono_features())
    }
}

impl<T: gpui::Styled> MonoStyled for T {}

impl Theme {
    /// The code [`gpui::Font`] for this theme — see [`mono_font`].
    pub fn mono(&self) -> gpui::Font {
        mono_font(self.font_mono.clone())
    }

    // ---- numbers drive layout (px) ----
    /// Frost translucency over the blurred window background (macOS vibrancy).
    /// Opaque elsewhere: Linux/Windows get no compositor-blur guarantee, and a
    /// merely transparent window would show raw desktop through the sidebar.
    /// Darkness matched by eye to a reference Electron app's dark glass. That
    /// scrim is 0.76 over `hsl(0 0% 3%)`, but it sits on Electron's
    /// `under-window` vibrancy MATERIAL, which pre-darkens the blur; our bare
    /// backdrop blur has no material layer, so the scrim runs heavier to land
    /// on the same perceived tone (see [`Theme::glass`]).
    pub const GLASS_ALPHA: f32 = if cfg!(target_os = "macos") { 0.80 } else { 1.0 };
    /// Light-mode frost alpha — glass-forward, like dark mode.
    ///
    /// A light tint controls the blur less than a dark one: the desktop's
    /// colour bleeds through more readily, so light frost runs *heavier* than
    /// an equal-looking dark frost to keep the chrome on a known-enough
    /// background for its labels (macOS light sidebars do the same — their
    /// vibrancy material is mostly white). Floating cards compensate further:
    /// see [`Self::glass_overlay`], where light coverage steps up to keep menu
    /// text legible over an unknown backdrop.
    pub const GLASS_ALPHA_LIGHT: f32 = if cfg!(target_os = "macos") { 0.80 } else { 1.0 };
    /// The Windows caption band (caption buttons, their drag strip, and the
    /// rail clearance under them). The macOS/Linux chrome lines up on
    /// `shell::CHROME_CENTER_Y` instead.
    pub const TITLEBAR_HEIGHT: f32 = 38.0;
    /// Reserved status strip under the content outlet (zeron `h-6`) — the
    /// WorkingIndicator row; reserving it keeps the composer from shifting.
    pub const STATUS_STRIP_HEIGHT: f32 = 24.0;
    /// Height of the gradient that fades the transcript into the panel
    /// background at its bottom edge. The transcript's last row must pad
    /// itself past this band so settled content (message text, the
    /// hover-revealed timestamp) never sits inside the fade when scrolled
    /// to the bottom.
    pub const TRANSCRIPT_FADE_BAND: f32 = 24.0;
    /// Message bubble corner radius.
    pub const BUBBLE_RADIUS: f32 = 16.0;
    /// Small control radius (buttons, chips).
    pub const CONTROL_RADIUS: f32 = 6.0;
    /// Base spacing steps.
    pub const SPACE_SM: f32 = 8.0;
    pub const SPACE_MD: f32 = 12.0;
    pub const SPACE_LG: f32 = 16.0;

    /// The frost tint painted over the blurred window background (macOS glass).
    /// Dark: darker than `surface`, a charcoal frost over the blurred desktop.
    /// Light: a near-white frost run heavier than dark's — see
    /// [`Self::GLASS_ALPHA_LIGHT`]. On opaque platforms this IS the surface
    /// tone (no tint swap). These backing tones and opacity are fixed for
    /// each appearance, independent of overall presets and card overrides.
    pub fn glass(&self) -> Hsla {
        match self.appearance {
            Appearance::Dark => {
                if Self::GLASS_ALPHA < 1.0 {
                    grey(24).opacity(Self::GLASS_ALPHA)
                } else {
                    grey(28)
                }
            }
            Appearance::Light => {
                if Self::GLASS_ALPHA_LIGHT < 1.0 {
                    // 0xfa, not the surface's 0xf4-ish grey: at 90% coverage
                    // the tint IS the sidebar tone, and the darker grey read
                    // as a dingy pane next to the white content card.
                    grey(0xfa).opacity(Self::GLASS_ALPHA_LIGHT)
                } else {
                    neutral(0.968)
                }
            }
        }
    }

    /// Whether this appearance paints translucent chrome over the blurred
    /// desktop. Glass-only recipes — backdrop blurs, translucent popover
    /// tints, per-glyph edge fades — must gate on this, not on
    /// [`Self::GLASS_ALPHA`]: that constant is platform-wide, while the frost
    /// alpha (and with it whether glass is on at all) is per-appearance.
    pub fn is_glass(&self) -> bool {
        self.glass().a < 1.0
    }

    /// Hover wash for chrome that sits ON GLASS (sidebar rows, tabs, titlebar
    /// buttons). One recipe, both appearances: the 11% [`wash`], tone-flipped
    /// by the palette convention (soft-white on dark, soft-black on light).
    ///
    /// Hover and selection share the SAME fill (selection adds only the ring).
    /// Light previously ran heavy white washes here (hover 0.55, selection
    /// 0.92) after a black-hover-next-to-white-selection mismatch report; now
    /// hover and selection are *both* the tone-flipped wash, so they lift the
    /// same way again. Light's alpha sits under dark's: dark's 11% at the
    /// light tone read too dark over the bright frost (user report).
    pub fn glass_hover(&self) -> Hsla {
        match self.appearance {
            Appearance::Dark => wash_for(Appearance::Dark, 0.11),
            Appearance::Light => wash_for(Appearance::Light, 0.06),
        }
    }

    /// The translucent tint floating cards paint over their backdrop blur
    /// (see [`crate::kit::frost::frosted`]). Dark: a mid-charcoal frost
    /// (`oklch(0.40 0 0 / 38%)`) so menus lift off the panel without reading
    /// as black slabs. Light: heavier — a translucent white tint left menu
    /// text ghosting over whatever sat behind the popover, so light coverage
    /// steps up to keep rows on a known background.
    pub fn glass_overlay(&self) -> Hsla {
        match self.appearance {
            Appearance::Dark => oklch(0.40, 0.0, 0.0).opacity(0.38),
            Appearance::Light => self.surface_overlay.opacity(0.85),
        }
    }

    /// The composer pill / question panel fill. Light's `input_bg` is opaque
    /// white (the elevation ladder on an opaque page) — over glass it read as
    /// a solid slab in front of the frosted blur, so it thins to a
    /// translucent tint there (0.6 and then 0.45 both still read too bright
    /// over the 0.80 frost — lowered on user request). Dark's 5% white wash
    /// is already glass-native.
    pub fn input_glass_bg(&self) -> Hsla {
        if self.is_glass() && matches!(self.appearance, Appearance::Light) {
            self.input_bg.opacity(0.30)
        } else {
            self.input_bg
        }
    }

    /// Small composer accessories float directly over scrolling text. Give
    /// their blur a visible but translucent tint: the blur softens underlying
    /// glyphs without making the pill look solid. Opaque platforms keep a
    /// solid backing.
    pub fn composer_accessory_bg(&self) -> Hsla {
        if self.is_glass() {
            self.surface_overlay.opacity(0.45)
        } else {
            self.surface_overlay
        }
    }

    /// The composer pill's lift shadow at its densest ([`crate::kit::soft_shadow`]):
    /// faint on light surfaces, deeper on dark ones where a light shadow
    /// would vanish.
    pub fn lift_shadow(&self) -> Hsla {
        match self.appearance {
            Appearance::Dark => hsla(0.0, 0.0, 0.0, 0.30),
            Appearance::Light => hsla(0.0, 0.0, 0.0, 0.14),
        }
    }

    /// Section-card fill (settings cards and similar in-panel cards). The
    /// opaque `surface` tone read as a harsh solid slab floating on the
    /// frosted blur (user report), so glass thins it to a translucent tint;
    /// opaque platforms keep the true card tone.
    pub fn card_glass_bg(&self) -> Hsla {
        if self.is_glass() {
            self.surface.opacity(0.40)
        } else {
            self.surface
        }
    }

    /// Sidebar project-card elevation: the Tailwind `shadow-sm` pair plus a
    /// 1px INSET hairline ring. The card fill sits only a few levels off the
    /// fixed translucent sidebar frost (and presets or a card override can
    /// land on it exactly), so the ring gives the card an edge independent of
    /// fill contrast; a dark drop shadow alone vanishes on dark glass.
    pub fn sidebar_card_shadows(&self) -> Vec<gpui::BoxShadow> {
        let ring = match self.appearance {
            Appearance::Dark => self.hairline(0.08),
            Appearance::Light => hsla(0.0, 0.0, 0.0, 0.07),
        };
        let drop = |blur: f32, spread: f32| gpui::BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.1),
            offset: gpui::point(gpui::px(0.0), gpui::px(1.0)),
            blur_radius: gpui::px(blur),
            spread_radius: gpui::px(spread),
            inset: false,
        };
        vec![
            drop(3.0, 0.0),
            drop(2.0, -1.0),
            gpui::BoxShadow {
                color: ring,
                offset: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                blur_radius: gpui::px(0.0),
                spread_radius: gpui::px(1.0),
                inset: true,
            },
        ]
    }

    /// The standard modal backdrop — see [`scrim`].
    pub fn scrim(&self) -> Hsla {
        scrim_for(self.appearance, SCRIM_ALPHA_DARK)
    }

    /// How the platform should composite the window behind our paint.
    ///
    /// Only dark macOS wants the blurred desktop — light chrome is opaque by
    /// design ([`Self::GLASS_ALPHA_LIGHT`]), so it keeps opaque compositing
    /// (subpixel-friendly, no vibrancy cost for a blur nothing shows). This is
    /// a method rather than a constant because it has to be *re-applied* after
    /// every theme swap: gpui's macOS backend tears the `NSVisualEffectView`
    /// out of the hierarchy whenever the value is anything but `Blurred`, and
    /// the re-apply in `appearance::apply` is what restores vibrancy when the
    /// user switches back to dark. See zed's `crates/zed/src/main.rs`, which
    /// runs the same loop on every settings change.
    pub fn window_background_appearance(&self) -> gpui::WindowBackgroundAppearance {
        if self.is_glass() {
            gpui::WindowBackgroundAppearance::Blurred
        } else {
            gpui::WindowBackgroundAppearance::Opaque
        }
    }

    /// Build the dark theme. Surfaces sit a step up from OLED black so the
    /// window reads as charcoal rather than a void; the elevation ladder is
    /// the same shape as before, just lifted.
    pub fn dark() -> Self {
        Self {
            regions: Default::default(),
            markdown: MarkdownMetrics::default(),
            text_style_revision: 0,
            markdown_link: None,
            user_bubble: None,
            code_block_background: None,
            code_block_text: None,
            inline_code_text: None,
            inline_code_background: None,
            appearance: Appearance::Dark,
            bg: grey(22),      // main panel — #161616
            surface: grey(28), // shell / sidebar — #1c1c1c
            surface_raised: grey(0x3c),
            surface_card: grey(0x1e),
            surface_dialog: grey(0x22),
            surface_overlay: grey(0x2a),
            element_hover: hsla(0.0, 0.0, 0.92, 0.11),
            element_active: hsla(0.0, 0.0, 0.92, 0.16),
            border: hsla(0.0, 0.0, 1.0, 0.08),
            border_strong: hsla(0.0, 0.0, 1.0, 0.14),
            text: neutral(0.96),
            text_muted: neutral(0.74),
            text_faint: neutral(0.59),
            text_dim: grey(0x98),
            solid: neutral(0.922),                       // near-white plate
            on_solid: grey(0x0e),                        // near-black label
            accent: oklch(0.673, 0.182, 276.935),        // indigo-400
            accent_strong: oklch(0.585, 0.233, 277.117), // indigo-500
            danger: oklch(0.704, 0.191, 22.216),         // red-400
            danger_muted: oklch(0.808, 0.114, 19.571),   // red-300
            warning: oklch(0.828, 0.189, 84.429),        // amber-400
            warning_muted: oklch(0.924, 0.12, 95.746),   // amber-200
            success: oklch(0.765, 0.177, 163.223),       // emerald-400
            busy: oklch(0.718, 0.202, 349.761),          // pink-400
            success_muted: oklch(0.845, 0.143, 164.978), // emerald-300
            surface_raised_hover: grey(0x4a),
            input_bg: hsla(0.0, 0.0, 1.0, 0.05),
            selection: hsla(0.66, 0.6, 0.55, 0.35),
            cursor: hsla(0.0, 0.0, 1.0, 0.35),
            caret: hsla(0.66, 0.7, 0.7, 1.0),
            danger_strong: oklch(0.58, 0.16, 25.0),
            code_text: oklch(0.845, 0.143, 164.978), // emerald-300
            code_wash: oklch(0.765, 0.177, 163.223).opacity(0.12), // emerald-400/12
            syntax: SyntaxPalette::dark(neutral(0.96), neutral(0.62), oklch(0.704, 0.191, 22.216)),
            diff_add: oklch(0.765, 0.177, 163.223), // emerald-400
            diff_del: oklch(0.704, 0.191, 22.216),  // red-400
            diff_hunk_bg: hsla(0.6, 0.35, 0.6, 0.05),
            font_sans: "Geist".into(),
            font_mono: "Geist Mono".into(),
        }
    }

    /// Build the light theme.
    ///
    /// Neutrals are the same oklch scale read from the other end, but the *roles*
    /// are reassigned rather than mirrored (see the module docs): content plane
    /// white, chrome grey, raised surfaces white-plus-shadow. Text tones are
    /// picked to reproduce the dark theme's contrast ratios, and accents drop
    /// from the 400 to the 600 step at identical hue so they clear WCAG AA on
    /// white instead of glowing.
    pub fn light() -> Self {
        Self {
            regions: Default::default(),
            markdown: MarkdownMetrics::default(),
            text_style_revision: 0,
            markdown_link: None,
            user_bubble: None,
            code_block_background: None,
            code_block_text: None,
            inline_code_text: None,
            inline_code_background: None,
            appearance: Appearance::Light,
            bg: grey(0xff), // main panel — clean white
            // Deeper than ~neutral-100 looks on paper: the content card is pure
            // white and sits *inside* this surface, so too small a step leaves the
            // whole window one flat sheet with a hairline drawn on it.
            surface: neutral(0.968),
            // A real grey, NOT white. This is the opaque-plate tone — user
            // message bubbles, the jump-to-bottom pill — and those sit directly
            // on the white content plane with no border or shadow to save them.
            // White here made the user's own messages vanish into the page.
            // Popovers do not use this; they have their own ladder below.
            surface_raised: neutral(0.940),
            surface_card: grey(0xff),
            surface_dialog: grey(0xff),
            surface_overlay: grey(0xff),
            element_hover: hsla(0.0, 0.0, 0.10, 0.06),
            element_active: hsla(0.0, 0.0, 0.10, 0.10),
            border: hsla(0.0, 0.0, 0.0, 0.10),
            border_strong: hsla(0.0, 0.0, 0.0, 0.17),
            // ~neutral-850. Pure neutral-900 measures 17.9:1 on white — *more*
            // contrast than dark mode's 16.1:1, which reads as harsh rather than
            // crisp. Backing off to 0.25 lands at ~16:1: the same perceived
            // weight as the dark theme, not the maximum available.
            text: neutral(0.25),
            text_muted: neutral(0.439), // ~neutral-600 → ~7.7:1
            // A touch darker than dark mode's neutral-500 counterpart: the light
            // sidebar is a real grey, and faint text has to clear its floor there
            // too, not just on the white content plane.
            text_faint: neutral(0.535),
            text_dim: neutral(0.50),
            solid: neutral(0.205),    // near-black plate, deeper than body text
            on_solid: neutral(0.985), // near-white label
            accent: oklch(0.511, 0.262, 276.966), // indigo-600
            accent_strong: oklch(0.511, 0.262, 276.966), // indigo-600 fill
            danger: oklch(0.577, 0.245, 27.325), // red-600
            danger_muted: oklch(0.505, 0.213, 27.518), // red-700
            warning: oklch(0.555, 0.163, 48.998), // amber-700 — carries 12px text
            warning_muted: oklch(0.473, 0.137, 46.201), // amber-800
            success: oklch(0.596, 0.145, 163.225), // emerald-600
            busy: oklch(0.592, 0.249, 0.584), // pink-600
            success_muted: oklch(0.508, 0.118, 165.612), // emerald-700
            // Opaque pills darken on hover here rather than brighten — same
            // "brighten the plate, don't wash it out" rule, read the other way.
            surface_raised_hover: neutral(0.900),
            // A recessed strip on white needs far less ink than on near-black;
            // the dark 16% would read as a bruise.
            input_bg: grey(0xff),
            selection: hsla(0.66, 0.75, 0.62, 0.28),
            cursor: hsla(0.0, 0.0, 0.0, 0.55),
            caret: hsla(0.66, 0.78, 0.42, 1.0),
            danger_strong: oklch(0.51, 0.20, 25.0),
            code_text: oklch(0.508, 0.118, 165.612), // emerald-700
            code_wash: oklch(0.596, 0.145, 163.225).opacity(0.10), // emerald-600/10
            syntax: SyntaxPalette::light(neutral(0.25), neutral(0.48), oklch(0.505, 0.213, 27.518)),
            diff_add: oklch(0.596, 0.145, 163.225), // emerald-600
            diff_del: oklch(0.577, 0.245, 27.325),  // red-600
            diff_hunk_bg: hsla(0.6, 0.35, 0.35, 0.07),
            font_sans: "Geist".into(),
            font_mono: "Geist Mono".into(),
        }
    }

    /// Build the theme for an appearance.
    pub fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Dark => Self::dark(),
            Appearance::Light => Self::light(),
        }
    }

    /// Read the theme global.
    pub fn of(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    /// Overlay ink at `alpha` — see [`ink`].
    pub fn ink(&self, alpha: f32) -> Hsla {
        ink_for(self.appearance, alpha)
    }

    /// Hairline ink at `alpha` — see [`hairline`].
    pub fn hairline(&self, alpha: f32) -> Hsla {
        hairline_for(self.appearance, alpha)
    }

    /// State wash at `alpha` — see [`wash`].
    pub fn wash(&self, alpha: f32) -> Hsla {
        wash_for(self.appearance, alpha)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Global for Theme {}

/// A neutral (chroma 0) oklch tone as Hsla. Chroma 0 means r == g == b exactly,
/// so this goes straight to an achromatic Hsla (skipping the hue math avoids
/// float-noise saturation).
pub fn neutral(lightness: f32) -> Hsla {
    let [v, _, _] = oklch_to_srgb(lightness, 0.0, 0.0);
    hsla(0.0, 0.0, v, 1.0)
}

/// Translucent **fill** ink for interactive states and chip plates: soft-white on
/// dark, soft-black on light at [`INK_FILL_SCALE`] of the alpha.
///
/// Alphas are quoted in *dark-mode terms* at every call site — the dark theme is
/// the tuned one — and the light value is derived. Callers keep one number and
/// both appearances stay in the relationship the dark tuning established.
///
/// Fills must never rest on transparent BLACK in dark mode: fully opaque washes
/// killed the glass and flashed dark mid-fade (user reports), so hover fades rest
/// on `ink(0.0)`, which stays tonally correct at zero alpha.
pub fn ink(alpha: f32) -> Hsla {
    ink_for(current_appearance(), alpha)
}

fn ink_for(appearance: Appearance, alpha: f32) -> Hsla {
    match appearance {
        // Soft-white, not pure white: alphas are high enough to stay visible at
        // the brightest backdrop the 0.90 glass scrim can produce.
        Appearance::Dark => hsla(0.0, 0.0, 1.0, alpha),
        Appearance::Light => hsla(0.0, 0.0, 0.0, alpha * INK_FILL_SCALE),
    }
}

/// Translucent **hairline** ink for borders, dividers and rings: white on dark,
/// black on light at [`INK_HAIRLINE_SCALE`] of the alpha.
///
/// Separate from [`ink`] because edges and fills scale in opposite directions
/// when the field brightens — a 1px line needs *more* ink on white, a plate needs
/// less.
pub fn hairline(alpha: f32) -> Hsla {
    hairline_for(current_appearance(), alpha)
}

fn hairline_for(appearance: Appearance, alpha: f32) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 1.0, alpha),
        Appearance::Light => hsla(0.0, 0.0, 0.0, (alpha * INK_HAIRLINE_SCALE).min(0.5)),
    }
}

/// Interactive-state wash: a softened [`ink`] that stops short of pure black or
/// white so hover plates read as tinted glass rather than paint.
pub fn wash(alpha: f32) -> Hsla {
    wash_for(current_appearance(), alpha)
}

fn wash_for(appearance: Appearance, alpha: f32) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 0.92, alpha),
        Appearance::Light => hsla(0.0, 0.0, 0.10, alpha * INK_FILL_SCALE),
    }
}

/// Alpha of the standard modal backdrop in dark mode. Call sites that need a
/// heavier or lighter scrim pass their own dark-mode alpha to [`scrim`].
pub const SCRIM_ALPHA_DARK: f32 = 0.60;

/// Modal backdrop at `alpha_dark` (quoted, as everywhere, in dark-mode terms).
///
/// Black in both appearances — a scrim's job is to darken what is behind it, and
/// a "light scrim" of white would wash the modal out rather than seat it. What
/// changes is strength: on a bright field a dark-mode-weight scrim reads as a
/// blackout, so light mode scales to roughly half.
pub fn scrim(alpha_dark: f32) -> Hsla {
    scrim_for(current_appearance(), alpha_dark)
}

fn scrim_for(appearance: Appearance, alpha_dark: f32) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 0.0, alpha_dark),
        Appearance::Light => hsla(0.0, 0.0, 0.0, 0.32 * (alpha_dark / SCRIM_ALPHA_DARK)),
    }
}

/// Recessed band behind a palette/picker header or footer strip. Translucent
/// so the glass still reads through.
pub fn band() -> Hsla {
    band_for(current_appearance())
}

fn band_for(appearance: Appearance) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 0.0, 0.16),
        // A recessed strip on white needs far less ink than on near-black; the
        // dark 16% would read as a bruise.
        Appearance::Light => hsla(0.0, 0.0, 0.0, 0.045),
    }
}

/// Selected/keyboard-active treatment for rows and chips INSIDE a floating
/// card (menu rows, the picker rail, segmented chips). The card is already the
/// bright plane in light mode, so a white lift can't read there — selection is
/// the tone-flipped grey wash, at 6% (dark's 11% read too dark on the bright
/// plane, user report).
pub fn card_selected_bg() -> Hsla {
    match current_appearance() {
        Appearance::Dark => wash(0.11),
        Appearance::Light => wash(0.06),
    }
}

/// Selection outline for rows and chips INSIDE a floating card (menu rows,
/// the picker rail, segmented chips): the inset ring alone, in both
/// appearances. Card rows fill with a translucent wash
/// ([`card_selected_bg`]), and a drop shadow — a filled rect painted BEHIND
/// the element — shows straight through a translucent fill as a grey plate,
/// and gpui paints inset shadows ON TOP of the background, edges only — a
/// border with zero layout cost. The card already carries the elevation
/// shadow; selection inside it only needs the edge. Light pins the ring at a
/// flat 7% black rather than the scaled hairline: heavier rings outlined every
/// selected chip in a dark box — the ring should define the chip, not frame it.
pub fn card_selected_shadows() -> Vec<gpui::BoxShadow> {
    let color = match current_appearance() {
        Appearance::Dark => hairline(0.09),
        Appearance::Light => hsla(0.0, 0.0, 0.0, 0.07),
    };
    vec![gpui::BoxShadow {
        color,
        offset: gpui::point(gpui::px(0.0), gpui::px(0.0)),
        blur_radius: gpui::px(0.0),
        spread_radius: gpui::px(1.0),
        inset: true,
    }]
}

/// An exact achromatic tone from an 8-bit channel value (`grey(13)` ≡ `#0d0d0d`)
/// — for surfaces matched against reference-screenshot samples.
pub fn grey(value: u8) -> Hsla {
    hsla(0.0, 0.0, value as f32 / 255.0, 1.0)
}

/// Convert an oklch color (CSS notation: L 0..1, C, H in degrees) to gpui Hsla.
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Hsla {
    let [r, g, b] = oklch_to_srgb(l, c, h_deg);
    let (h, s, l) = rgb_to_hsl(r, g, b);
    hsla(h, s, l, 1.0)
}

/// oklch → sRGB (each 0..1, clamped/gamut-clipped per channel).
/// Reference: Björn Ottosson's OKLab definition (the same matrices CSS Color 4 uses).
pub fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> [f32; 3] {
    let h = h_deg.to_radians();
    let a = c * h.cos();
    let b = c * h.sin();

    // OKLab → LMS (cube roots undone)
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);

    // LMS → linear sRGB
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_93 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_4 * s3;
    let b = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;

    [gamma_encode(r), gamma_encode(g), gamma_encode(b)]
}

fn gamma_encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB (0..1 components) → HSL, all components 0..1 (gpui's Hsla convention).
pub fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let delta = max - min;
    if delta < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    (h, s, l)
}

/// HSL (gpui convention, all 0..1) → sRGB components 0..1.
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= f32::EPSILON {
        return [l, l, l];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// WCAG 2.1 relative luminance of an opaque color.
pub fn relative_luminance(color: Hsla) -> f32 {
    let lin = |c: f32| {
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = hsl_to_rgb(color.h, color.s, color.l);
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG 2.1 contrast ratio between two opaque colors (1.0 … 21.0).
///
/// Used by the palette tests to prove each light token reproduces the contrast
/// its dark counterpart had, rather than merely looking plausible.
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Composite `fg` (which may be translucent) over an opaque `bg`, returning the
/// opaque result — the color the eye actually receives.
#[cfg(test)]
pub fn flatten(fg: Hsla, bg: Hsla) -> Hsla {
    let a = fg.a.clamp(0.0, 1.0);
    let [fr, fg_, fb] = hsl_to_rgb(fg.h, fg.s, fg.l);
    let [br, bg_, bb] = hsl_to_rgb(bg.h, bg.s, bg.l);
    let (h, s, l) = rgb_to_hsl(
        fr * a + br * (1.0 - a),
        fg_ * a + bg_ * (1.0 - a),
        fb * a + bb * (1.0 - a),
    );
    hsla(h, s, l, 1.0)
}

/// Scoped inline-code tint. Defaults to emerald text and a translucent wash;
/// overrides do not change the shared tokens used by mention chips.
pub fn inline_code_text(theme: &Theme) -> Hsla {
    theme.inline_code_text.unwrap_or(theme.code_text)
}
pub fn inline_code_wash(theme: &Theme) -> Hsla {
    theme.inline_code_background.unwrap_or(theme.code_wash)
}
/// Fenced code block fill: the chat override, else a faint ink wash.
pub fn code_block_background(theme: &Theme) -> Hsla {
    theme
        .code_block_background
        .unwrap_or_else(|| theme.ink(0.035))
}

#[cfg(test)]
mod tests;
