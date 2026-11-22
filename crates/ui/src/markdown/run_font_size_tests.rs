//! The (local) gpui fork's per-run `TextRun::font_size`, checked with the
//! real platform text system and the bundled Geist / Geist Mono: inline code
//! typeset at 0.86em inside one prose line, as real text.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use gpui::{Font, Pixels, TextRun, TextSystem, WindowTextSystem, font, px};

const PROSE: Pixels = px(16.);
const CODE_SCALE: f32 = 0.86;

/// One platform text system for all tests: building (or dropping) headless
/// macOS platforms on parallel test threads aborts the process.
fn text_system() -> WindowTextSystem {
    static TEXT_SYSTEM: OnceLock<Arc<TextSystem>> = OnceLock::new();
    let text_system = TEXT_SYSTEM.get_or_init(|| {
        let platform = gpui_platform::current_platform(true);
        let text_system = Arc::new(TextSystem::new(platform.text_system()));
        std::mem::forget(platform);
        text_system
            .add_fonts(vec![
                Cow::Borrowed(crate::FONT_GEIST),
                Cow::Borrowed(crate::FONT_GEIST_MONO),
            ])
            .expect("bundled fonts load");
        text_system
    });
    WindowTextSystem::new(text_system.clone())
}

fn run(len: usize, font: Font, font_size: Option<Pixels>) -> TextRun {
    TextRun {
        len,
        font,
        font_size,
        ..Default::default()
    }
}

/// "call " + `run_test` + " now": prose, code, prose.
const BEFORE: &str = "call ";
const CODE: &str = "run_test";
const AFTER: &str = " now";

fn line_runs(code_size: Option<Pixels>) -> Vec<TextRun> {
    vec![
        run(BEFORE.len(), font("Geist"), None),
        run(CODE.len(), font("Geist Mono"), code_size),
        run(AFTER.len(), font("Geist"), None),
    ]
}

#[test]
fn inline_code_run_at_086em_is_narrower_and_keeps_the_line_metrics() {
    let text_system = text_system();
    let text = format!("{BEFORE}{CODE}{AFTER}");
    let full = text_system.layout_line(&text, PROSE, &line_runs(None), None);
    let small = text_system.layout_line(&text, PROSE, &line_runs(Some(PROSE * CODE_SCALE)), None);

    let code_start = BEFORE.len();
    let code_end = code_start + CODE.len();
    let code_width =
        |layout: &gpui::LineLayout| layout.x_for_index(code_end) - layout.x_for_index(code_start);
    let (full_code, small_code) = (code_width(&full), code_width(&small));
    assert!(
        (f32::from(small_code) - f32::from(full_code) * CODE_SCALE).abs() < 0.1,
        "code run {small_code:?} vs {full_code:?} at 1em"
    );
    // The prose before the code is untouched; the line is narrower by the
    // code run's 14%.
    assert_eq!(small.x_for_index(code_start), full.x_for_index(code_start));
    assert!(small.width < full.width);
    let saved = f32::from(full.width) - f32::from(small.width);
    assert!((saved - f32::from(full_code) * (1. - CODE_SCALE)).abs() < 0.1);

    // The code run is shaped (and will be painted) at its own size.
    assert_eq!(
        small.font_size_for_index(code_start),
        Some(PROSE * CODE_SCALE)
    );
    assert_eq!(small.font_size_for_index(0), Some(PROSE));
    // The prose at 1em sets the line's ascent/descent: one shared baseline.
    assert_eq!(small.ascent, full.ascent);
    assert_eq!(small.descent, full.descent);

    // Hit-testing maps back across both size changes.
    for index in 0..text.len() {
        let x = small.x_for_index(index);
        assert_eq!(small.index_for_x(x), Some(index), "index_for_x at {index}");
        assert_eq!(small.closest_index_for_x(x), index, "closest at {index}");
    }
}

#[test]
fn wrapping_uses_the_smaller_code_width() {
    let text_system = text_system();
    let text = format!("{BEFORE}{CODE}{AFTER}");
    let full = text_system.layout_line(&text, PROSE, &line_runs(None), None);
    // A wrap width between the two line widths: only the 1em line wraps.
    let small_width = text_system
        .layout_line(&text, PROSE, &line_runs(Some(PROSE * CODE_SCALE)), None)
        .width;
    let wrap_width = (full.width + small_width) / 2.;
    let wrapped = |code_size| {
        text_system
            .shape_text(
                text.clone().into(),
                PROSE,
                &line_runs(code_size),
                Some(wrap_width),
                None,
            )
            .unwrap()[0]
            .wrap_boundaries()
            .len()
    };
    assert_eq!(wrapped(None), 1);
    assert_eq!(wrapped(Some(PROSE * CODE_SCALE)), 0);
}
