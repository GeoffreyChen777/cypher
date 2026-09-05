//! Settings → Appearance: pick between following the system and pinning light or
//! dark.
//!
//! Uses [`widgets::option_card_row`] — a preview-card picker, because the choice
//! is a *look*, and a miniature of the result says more than a sentence about it.
//! The control itself is theme-agnostic; only the previews below know what a
//! theme is.
//!
//! Theme selection plus client-local chat typography, spacing and colors.

mod chat;
mod color_picker;
mod surfaces;

use gpui::{
    AnyElement, Context, Hsla, IntoElement, Render, SharedString, Window, div, prelude::*, px,
};

use crate::appearance::{self, AppearanceMode};
use crate::settings::widgets;
use crate::theme::Theme;

const SECTION_SPACING: f32 = 24.0;
const SECTION_BODY_GAP: f32 = 12.0;

/// Unboxed groups and rows, local to Appearance rather than changing the
/// card-based layouts used by other settings pages.
fn content_group() -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(SECTION_BODY_GAP))
        .mt(px(SECTION_SPACING))
}

fn setting_row() -> gpui::Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(12.0))
        .py(px(10.0))
}

fn section_frame(theme: &Theme, first: bool) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .mt(px(SECTION_SPACING))
        .when(!first, |el| {
            el.border_t_1()
                .border_color(theme.border)
                .pt(px(SECTION_SPACING))
        })
}

fn section_header(
    theme: &Theme,
    title: &'static str,
    expanded: bool,
    action: Option<AnyElement>,
    toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(format!("appearance-section-{title}")))
        .h(px(36.0))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .pr(px(8.0))
        .cursor_pointer()
        .on_click(toggle)
        .child(widgets::field_label(theme, title).flex_1())
        .children(action)
        .child(
            crate::icons::icon(if expanded {
                crate::icons::ALT_ARROW_DOWN
            } else {
                crate::icons::ALT_ARROW_RIGHT
            })
            .size(px(14.0))
            .text_color(theme.text_muted),
        )
        .into_any_element()
}

pub struct AppearancePage {
    mode_expanded: bool,
    chat: gpui::Entity<chat::ChatStyleEditor>,
    overall: gpui::Entity<surfaces::SurfaceStyleEditor>,
    regions: Vec<gpui::Entity<surfaces::SurfaceStyleEditor>>,
}

impl AppearancePage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            mode_expanded: true,
            chat: cx.new(chat::ChatStyleEditor::new),
            overall: cx.new(|cx| surfaces::SurfaceStyleEditor::new(None, cx)),
            regions: crate::surface_style::Region::ALL
                .into_iter()
                .map(|r| cx.new(|cx| surfaces::SurfaceStyleEditor::new(Some(r), cx)))
                .collect(),
        }
    }
}

/// One placeholder bar in the miniature, width given as a fraction of its
/// container.
///
/// Relative rather than fixed px because the System card renders this same
/// miniature into *half* a card. Fixed widths were wider than the squeezed
/// content pane and spilled out over the card edge.
fn bar(fraction: f32, tone: Hsla) -> gpui::Div {
    div()
        .h(px(5.0))
        .w(gpui::relative(fraction))
        .rounded(px(3.0))
        .bg(tone)
}

/// Which corners a miniature rounds — the split card needs each half to round
/// only its outer side so the two meet flush down the middle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Corners {
    All,
    Left,
    Right,
}

/// A miniature of the app in `theme`: sidebar strip, inset content card, a few
/// placeholder lines. Built from the theme's own tokens rather than fixed
/// swatches, so the previews stay honest if the palette is retuned.
///
/// Rounds itself: the card frame cannot do it for us (see
/// [`widgets::OPTION_CARD_RADIUS`]). Only this root paints a background that
/// reaches the corners — the sidebar strip is transparent and the content card is
/// inset — so rounding here is enough.
fn miniature(theme: &Theme, corners: Corners) -> AnyElement {
    let line = theme.text.opacity(0.22);
    let strong = theme.text.opacity(0.34);
    let r = px(widgets::OPTION_CARD_RADIUS);
    let root = div().size_full().flex().flex_row().bg(theme.surface);
    let root = match corners {
        Corners::All => root.rounded(r),
        Corners::Left => root.rounded_tl(r).rounded_bl(r),
        Corners::Right => root.rounded_tr(r).rounded_br(r),
    };
    root.child(
        // Sidebar strip.
        div()
            .w(px(44.0))
            .h_full()
            .flex_none()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .px(px(8.0))
            .pt(px(14.0))
            .child(bar(0.70, strong))
            .child(bar(1.0, line))
            .child(bar(0.85, line))
            .child(bar(1.0, line)),
    )
    .child(
        // Inset content card — the same rounded plate the real shell floats.
        div()
            .flex_1()
            .min_w_0()
            .my(px(8.0))
            .mr(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.bg)
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .p(px(10.0))
            .child(bar(0.62, strong))
            .child(bar(0.88, line))
            .child(bar(0.76, line))
            .child(bar(0.52, line)),
    )
    .into_any_element()
}

/// The System card: light on the left, dark on the right. Each half is a
/// complete miniature clipped to its side, which is what makes the card read as
/// "whichever one the system is on".
fn miniature_split() -> AnyElement {
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(
            div()
                .w_1_2()
                .h_full()
                .overflow_hidden()
                .child(miniature(&Theme::light(), Corners::Left)),
        )
        .child(
            div()
                .w_1_2()
                .h_full()
                .overflow_hidden()
                .child(miniature(&Theme::dark(), Corners::Right)),
        )
        .into_any_element()
}

/// The preview graphic for a mode.
///
/// The one place `Theme::light()`/`Theme::dark()` are legitimately built outside
/// the installed global: a preview has to show the palette you are *not* using.
fn preview(mode: AppearanceMode) -> AnyElement {
    match mode {
        AppearanceMode::System => miniature_split(),
        AppearanceMode::Light => miniature(&Theme::light(), Corners::All),
        AppearanceMode::Dark => miniature(&Theme::dark(), Corners::All),
    }
}

impl Render for AppearancePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let current = appearance::mode(cx);

        let cards = AppearanceMode::ALL.into_iter().map(|mode| {
            widgets::option_card(&theme, mode.label(), mode == current, preview(mode))
                .id(SharedString::from(format!("appearance-{}", mode.label())))
                .on_click(cx.listener(move |_, _, _, cx| {
                    appearance::set_mode(mode, cx);
                    cx.notify();
                }))
        });

        div()
            .id("appearance-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(&theme, "Appearance", None))
                    .child(
                        widgets::page_subtitle(
                            &theme,
                            "Choose the app theme and customize your chat interface. These settings stay on this client.",
                        )
                        .max_w(px(512.0))
                        .line_height(px(20.0)),
                    )
                    .child(
                        section_frame(&theme, true)
                            .child(section_header(&theme, "Appearance mode", self.mode_expanded, None,
                                cx.listener(|this, _, _, cx| {
                                    this.mode_expanded = !this.mode_expanded;
                                    cx.notify();
                                })))
                            .when(self.mode_expanded, |el| el.child(
                                widgets::option_card_row().mt(px(SECTION_BODY_GAP)).children(cards))),
                    )
                    .child(self.overall.clone())
                    .child(self.chat.clone())
                    .children(self.regions.iter().cloned()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_gets_a_card() {
        assert_eq!(AppearanceMode::ALL.len(), 3);
        for mode in AppearanceMode::ALL {
            assert!(!mode.label().is_empty());
        }
    }

    /// The previews must differ from each other, or the picker is decoration.
    /// Comparing the tones they are built from is the closest we can get without
    /// a renderer.
    #[test]
    fn light_and_dark_previews_draw_from_different_palettes() {
        let (l, d) = (Theme::light(), Theme::dark());
        assert_ne!(l.surface.l, d.surface.l);
        assert_ne!(l.bg.l, d.bg.l);
    }
}
