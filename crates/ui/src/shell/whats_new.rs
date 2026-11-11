//! The one-time "What's new" card ([`crate::whats_new`]): paged slides over
//! the Ready page, opened once per edition, after any first-run setup.

use gpui::{FocusHandle, ObjectFit};

use super::*;
use crate::kit::theme::hairline;
use crate::whats_new::{self, SLIDES, UPDATE_INTRO_VERSION};

/// The card's inner width; the picture fills it at 16:10.
const CONTENT_WIDTH: f32 = 520.0;
const PICTURE_HEIGHT: f32 = CONTENT_WIDTH * 10.0 / 16.0;
const PICTURE_RADIUS: f32 = 12.0;
const CARD_PADDING: f32 = 20.0;
/// Three lines of body copy, so the footer holds still between slides.
const BODY_HEIGHT: f32 = 57.0;

pub(super) struct WhatsNewUi {
    /// Decided at boot (main window only); opens on the first Ready render
    /// without the setup overlay.
    pub(super) due: bool,
    card: Option<WhatsNewCard>,
}

struct WhatsNewCard {
    page: usize,
    /// Tracked on the card so ←/→/return/esc reach it. Held while the card
    /// is open: boot focuses the composer after the card first renders.
    focus: FocusHandle,
}

impl WhatsNewUi {
    pub(super) fn new(due: bool) -> Self {
        Self { due, card: None }
    }
}

impl Shell {
    /// Opens the card when it is due, recording the edition right away: it
    /// shows once even if the app quits while it is open.
    fn open_whats_new_if_due(&mut self, cx: &mut Context<Self>) {
        if !self.whats_new.due || self.showing_setup() || SLIDES.is_empty() {
            return;
        }
        self.whats_new.due = false;
        self.whats_new.card = Some(WhatsNewCard {
            page: 0,
            focus: cx.focus_handle(),
        });
        self.settings.seen_update_intro = UPDATE_INTRO_VERSION;
        self.schedule_save(cx);
    }

    /// A new install finished first-run setup: it has no update to explain.
    pub(super) fn skip_whats_new_for_new_install(&mut self) {
        if !self.settings.setup_completed {
            self.settings.seen_update_intro = UPDATE_INTRO_VERSION;
            self.whats_new.due = false;
        }
    }

    fn close_whats_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.whats_new.card.take().is_some() {
            self.focus_landing(window, cx);
            cx.notify();
        }
    }

    /// Moves to slide `page`; past the last one closes the card.
    fn show_whats_new_page(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        if page >= SLIDES.len() {
            self.close_whats_new(window, cx);
        } else if let Some(card) = self.whats_new.card.as_mut() {
            card.page = page;
            cx.notify();
        }
    }

    fn whats_new_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.whats_new.card.as_ref().map(|card| card.page) else {
            return;
        };
        match key {
            "escape" => self.close_whats_new(window, cx),
            "right" | "enter" => self.show_whats_new_page(page + 1, window, cx),
            "left" if page > 0 => self.show_whats_new_page(page - 1, window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn render_whats_new_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.open_whats_new_if_due(cx);
        let card = self.whats_new.card.as_mut()?;
        if !card.focus.contains_focused(window, cx) {
            card.focus.focus(window, cx);
        }
        let page = card.page.min(SLIDES.len() - 1);
        let focus = card.focus.clone();
        let slide = &SLIDES[page];
        let last = page + 1 == SLIDES.len();
        let theme = Theme::of(cx).clone();

        let picture = match whats_new::image_path(page) {
            // Rounded itself: the frame's `overflow_hidden` clips square.
            Some(path) => gpui::img(path)
                .size_full()
                .rounded(px(PICTURE_RADIUS - 1.0))
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => div()
                .size(px(96.0))
                .rounded(px(28.0))
                .bg(theme.accent.opacity(0.16))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(slide.icon).size(px(48.0)).text_color(theme.accent))
                .into_any_element(),
        };
        let slide_body = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .h(px(PICTURE_HEIGHT))
                    .rounded(px(PICTURE_RADIUS))
                    .overflow_hidden()
                    .border_1()
                    .border_color(hairline(0.08))
                    .bg(theme.accent.opacity(0.06))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(picture),
            )
            .child(
                div()
                    .mt(px(18.0))
                    .text_size(px(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(slide.title)),
            )
            .child(
                popover::dialog_body(&theme, slide.body)
                    .mt(px(6.0))
                    .h(px(BODY_HEIGHT)),
            );

        let dots = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .children((0..SLIDES.len()).map(|index| {
                let current = index == page;
                div()
                    .id(("whats-new-dot", index))
                    .h(px(6.0))
                    .w(px(if current { 18.0 } else { 6.0 }))
                    .rounded_full()
                    .bg(if current {
                        theme.text
                    } else {
                        theme.text_faint.opacity(0.5)
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_whats_new_page(index, window, cx)
                    }))
            }));

        let mut buttons = div().flex().flex_row().items_center().gap(px(8.0));
        if !last {
            buttons = buttons.child(
                popover::btn_ghost(&theme, "Skip", "whats-new-skip")
                    .id("whats-new-skip")
                    .on_click(cx.listener(|this, _, window, cx| this.close_whats_new(window, cx))),
            );
        }
        if page > 0 {
            buttons = buttons.child(
                popover::btn_ghost(&theme, "Back", "whats-new-back")
                    .id("whats-new-back")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_whats_new_page(page - 1, window, cx)
                    })),
            );
        }
        buttons = buttons.child(
            popover::btn_primary(&theme, if last { "Get started" } else { "Next" })
                .id("whats-new-next")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show_whats_new_page(page + 1, window, cx)
                })),
        );

        let card = popover::dialog_card(&theme)
            .id("whats-new-dialog")
            .w(px(CONTENT_WIDTH + 2.0 * CARD_PADDING))
            .p(px(CARD_PADDING))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                this.whats_new_key(&ev.keystroke.key, window, cx)
            }))
            .child(
                div()
                    .mb(px(14.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "What's new in Cypher {}",
                        cypher_update::current_version()
                    ))),
            )
            .child(motion::fade_quick(("whats-new-page", page), slide_body))
            .child(
                div()
                    .mt(px(16.0))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(dots)
                    .child(buttons),
            );
        Some(popover::modal(
            "whats-new-dialog",
            viewport,
            card.into_any_element(),
        ))
    }
}
