//! Click-to-copy code blocks: the codes people copy or type somewhere else —
//! a pairing code, a sign-in device code, a command that carries one. The
//! whole block is the button; a trailing "Copy" turns into "Copied" for
//! [`COPIED_FOR`].
//!
//! The confirmation lives in a main-thread store keyed by the block's id
//! (like `motion`'s hover fades), so a page renders a block with one call
//! and no state of its own. [`copy`] is the same action for a separate
//! "Copy code" button next to a block.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, ClipboardItem, SharedString, Stateful, Window, div, prelude::*, px};

use crate::kit::icons;
use crate::kit::theme::{MonoStyled, Theme, ink};

/// How long a block reads "Copied" after a copy.
pub const COPIED_FOR: Duration = Duration::from_millis(1500);

thread_local! {
    static COPIED: RefCell<HashMap<SharedString, Instant>> = RefCell::new(HashMap::new());
}

/// Whether the block `key` was copied within the last [`COPIED_FOR`].
pub fn is_copied(key: &str) -> bool {
    COPIED.with(|copied| {
        let mut copied = copied.borrow_mut();
        copied.retain(|_, at| at.elapsed() < COPIED_FOR);
        copied.contains_key(key)
    })
}

/// Copy `text` and show "Copied" on block `key`, then repaint once the
/// confirmation has run out.
pub fn copy(key: impl Into<SharedString>, text: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    COPIED.with(|copied| copied.borrow_mut().insert(key.into(), Instant::now()));
    window.refresh();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(COPIED_FOR).await;
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

/// How big a block's code reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeSize {
    /// A code to read off and type in: large and semibold.
    Display,
    /// A command line: body-sized.
    Command,
}

/// A bordered monospace block showing `code`; clicking it copies `code`.
/// `id` must be unique on screen and stable across frames.
pub fn code_block(
    theme: &Theme,
    id: impl Into<SharedString>,
    code: impl Into<SharedString>,
    size: CodeSize,
) -> Stateful<gpui::Div> {
    let id = id.into();
    let code = code.into();
    let copied = is_copied(&id);
    let (text_size, weight) = match size {
        CodeSize::Display => (px(20.0), gpui::FontWeight::SEMIBOLD),
        CodeSize::Command => (px(12.5), gpui::FontWeight::NORMAL),
    };
    let key = id.clone();
    let text = code.clone();
    let tone = if copied {
        theme.success_muted
    } else {
        theme.text_muted
    };
    div()
        .id(id.clone())
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(format!("Copy {code}")))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .py(px(if size == CodeSize::Display { 8.0 } else { 6.0 }))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .bg(ink(0.025))
        .cursor_pointer()
        .hover(|s| s.bg(ink(0.05)))
        .on_click(move |_, window, cx| copy(key.clone(), &text, window, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .mono(theme)
                .text_size(text_size)
                .font_weight(weight)
                .text_color(theme.text)
                .child(code),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .text_size(px(11.5))
                .text_color(tone)
                .child(
                    icons::icon(if copied { icons::CHECK } else { icons::COPY })
                        .size(px(13.0))
                        .text_color(tone),
                )
                .child(SharedString::from(if copied { "Copied" } else { "Copy" })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_confirmation_is_per_block_and_runs_out() {
        COPIED.with(|copied| {
            copied.borrow_mut().insert("fresh".into(), Instant::now());
            copied
                .borrow_mut()
                .insert("stale".into(), Instant::now() - COPIED_FOR);
        });
        assert!(is_copied("fresh"));
        assert!(!is_copied("stale"));
        assert!(!is_copied("other"));
    }
}
