//! The project picker at the start of a workspace chat's Files and Git
//! headers: which of its roots ([`super::roots`]) the surface shows. Drawn
//! only for a chat with several roots; with a single choice (Git when one
//! project is a checkout) it is a plain label naming the root. A pick is
//! the chat's (Files and Git follow it together, see
//! [`crate::state::AppState::pick_file_root`]).

use cypher_proto::ChatFileRoot;
use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*, px};

use crate::kit::icons::{self, icon};
use crate::kit::motion;
use crate::kit::popover::{self, Popup};
use crate::kit::theme::Theme;
use crate::state::AppState;

/// The workspace's own folder wears the workspace glyph; projects a folder.
fn glyph(root: &ChatFileRoot) -> &'static str {
    if root.space_id.is_none() {
        icons::BOT
    } else {
        icons::FOLDER
    }
}

/// The row the label sits in, a menu's trigger or not.
fn frame(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(24.0))
        .px(px(7.0))
        .flex_shrink(1.0)
        .min_w(px(48.0))
        .max_w(px(180.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.0))
}

/// The shown root's glyph and name.
fn label(shown: &ChatFileRoot, theme: &Theme) -> [AnyElement; 2] {
    [
        icon(glyph(shown))
            .flex_none()
            .size(px(12.0))
            .text_color(theme.text_muted)
            .into_any_element(),
        div()
            .min_w_0()
            .truncate()
            .text_size(px(12.0))
            .text_color(theme.text)
            .child(SharedString::from(shown.name.clone()))
            .into_any_element(),
    ]
}

/// The trigger and, while open, its menu. `choices` are the roots the
/// surface may show, `shown` the one it shows; `menu` re-borrows the
/// surface's popup, whose current state is `mounted` ([`mounted`]). With
/// fewer than two choices: the label alone, no menu.
#[allow(clippy::too_many_arguments)]
pub fn render<V: 'static>(
    id: &'static str,
    choices: &[ChatFileRoot],
    shown: &ChatFileRoot,
    state: &Entity<AppState>,
    menu: fn(&mut V) -> &mut Popup<()>,
    mounted: Option<Option<std::time::Instant>>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> AnyElement {
    if choices.len() < 2 {
        return frame(id).children(label(shown, theme)).into_any_element();
    }
    let trigger = frame(id)
        .rounded(px(6.0))
        .cursor_pointer()
        .bg(motion::hover_blend(
            id,
            crate::kit::theme::wash(0.05),
            crate::kit::theme::wash(0.14),
        ))
        .on_hover(motion::hover_listener(id))
        .occlude()
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, _, window, _| {
                window.prevent_default();
                menu(this).note_trigger_press();
            }),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            if menu(this).take_press_was_open() {
                close(menu, cx, this);
            } else {
                menu(this).open(());
            }
            cx.notify();
        }))
        .children(label(shown, theme))
        .child(
            icon(icons::ALT_ARROW_DOWN)
                .flex_none()
                .size(px(12.0))
                .text_color(theme.text_muted.opacity(0.7)),
        );
    let Some(closing) = mounted else {
        return trigger.into_any_element();
    };
    let popup_theme = Theme::of(cx).clone();
    let rows = choices.iter().enumerate().map(|(ix, root)| {
        let pick = root.space_id.clone();
        let state = state.clone();
        let fade = format!("{id}-row-{ix}");
        popover::menu_row(&popup_theme, root.space_id == shown.space_id, fade)
            .id((SharedString::from(format!("{id}-row")), ix))
            .on_click(cx.listener(move |this, _, _, cx| {
                let pick = pick.clone();
                state.update(cx, |state, cx| state.pick_file_root(pick, cx));
                close(menu, cx, this);
            }))
            .child(
                icon(glyph(root))
                    .flex_none()
                    .size(px(13.0))
                    .text_color(popup_theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(root.name.clone())),
            )
    });
    let card = popover::popover_card(&popup_theme)
        .w(px(220.0))
        .on_mouse_down_out(cx.listener(move |this, _, _, cx| close(menu, cx, this)))
        .child(div().flex().flex_col().gap(px(2.0)).children(rows))
        .into_any_element();
    trigger
        .relative()
        .child(popover::anchored_menu_below_gap(
            SharedString::from(format!("{id}-menu")),
            card,
            closing,
            10.0,
        ))
        .into_any_element()
}

/// A popup's state as [`render`] takes it: `None` while unmounted, else
/// when its exit began.
pub fn mounted(menu: &Popup<()>) -> Option<Option<std::time::Instant>> {
    menu.get().map(|_| menu.closing_since())
}

fn close<V: 'static>(menu: fn(&mut V) -> &mut Popup<()>, cx: &mut Context<V>, this: &mut V) {
    if menu(this).begin_close() {
        popover::reap_popup(cx, menu);
    }
    cx.notify();
}
