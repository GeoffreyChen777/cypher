//! The shared floating Comment pill / anchored editor (round 20).
//!
//! One shell-level [`CommentPopup`] entity serves every surface that can
//! comment: the transcript's markdown text, a Git diff pane's code lines,
//! and terminal selections. A surface shows the pill by calling
//! [`CommentPopup::offer`] with the selected quote, a window-space anchor,
//! and a callback that clears *that surface's* selection wash on save/cancel.
//! The popup is rendered by the SHELL in a deferred layer above all clipped
//! surfaces, so it never gets cut off by a scroll container or pane edge.
//!
//! Saving emits [`CommentPopupEvent::CommentSaved`]; the shell subscribes
//! ONCE and forwards the comment into the current composer.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    AnyElement, App, Context, Entity, Pixels, Point, SharedString, Subscription, Window, div,
    point, prelude::*, px,
};

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::markdown::render;
use crate::markdown::selection::{self, SelectionScope};
use crate::theme::Theme;

/// Events the shell listens for.
pub enum CommentPopupEvent {
    /// A comment was saved in the anchored editor. `chat_id` is the chat that
    /// was selected when the selection SETTLED (captured at offer time by the
    /// surface), not the chat selected at emit time — the shell forwards it,
    /// and the composer's guard keeps the comment honest across a switch.
    CommentSaved {
        chat_id: String,
        quote: String,
        comment: String,
    },
}

impl gpui::EventEmitter<CommentPopupEvent> for CommentPopup {}

/// Which surface owns an offer — lets a surface dismiss ONLY its own comment
/// (a diff reload, or terminal output, must never hide the transcript's
/// pill).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentOwner {
    /// A markdown selection in a surface scope.
    Markdown(SelectionScope),
    /// A terminal selection, allocated a fresh id per panel so the drawer
    /// panel and the embedded right-pane panel never dismiss each other's
    /// pill.
    Terminal(u64),
}

impl CommentOwner {
    /// Allocate a fresh per-panel terminal owner id ([`CommentOwner::Terminal`]).
    pub fn next_terminal() -> CommentOwner {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        CommentOwner::Terminal(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A settled selection offer — the small `Comment` pill shown at the
/// selection endpoint while it waits for the click that turns it into an
/// editor.
#[derive(Clone)]
struct CommentOffer {
    /// The chat selected when the selection settled (forwarded on save so a
    /// mid-editor chat switch can't misroute the comment).
    chat_id: String,
    /// Normalized quoted text being commented on.
    quote: String,
    /// Window-space anchor from the mouse-up event.
    anchor: Point<Pixels>,
    /// Who owns the offer (scoped dismissal).
    owner: CommentOwner,
    /// Markdown surfaces: re-resolve the anchor from the frame registry each
    /// render (keeps the pill attached while the text scrolls/reflows).
    head: Option<CommentHead>,
    /// Drops the source surface's selection wash on save/cancel/dismiss.
    clear_selection: Rc<dyn Fn(&mut App)>,
}

/// The anchored comment editor replacing the pill once clicked: the
/// normalized quote preview and the window-space anchor.
#[derive(Clone)]
struct CommentEditor {
    chat_id: String,
    quote: String,
    anchor: Point<Pixels>,
    owner: CommentOwner,
    head: Option<CommentHead>,
    clear_selection: Rc<dyn Fn(&mut App)>,
}

/// Markdown-surface anchor bookkeeping: the element key + byte index of the
/// selection head, and the scope whose registry holds it. Surfaces construct
/// one when offering a markdown selection.
#[derive(Clone)]
pub struct CommentHead {
    pub key: String,
    pub ix: usize,
    pub scope: SelectionScope,
}

/// Normalize a selected quote for storage: trim outer whitespace and fold
/// NBSPs to plain spaces (a NBSP selection boundary must not become a
/// literal NBSP in the persisted quote).
pub fn normalize_quote(raw: &str) -> String {
    raw.trim().replace('\u{a0}', " ")
}

/// The shared floating Comment pill / editor, anchored at a settled
/// selection endpoint (window coordinates via gpui `anchored().position`).
/// The shell renders it above every clipped surface.
pub struct CommentPopup {
    offer: Option<CommentOffer>,
    editor: Option<CommentEditor>,
    /// Reusable multiline input for the anchored comment editor.
    comment_input: Entity<ComposerInput>,
    /// Input events for the comment editor: Enter saves.
    _comment_input_events: Subscription,
}

impl CommentPopup {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let comment_input = cx.new(|cx| ComposerInput::new("Add a comment…", cx));
        let comment_events = cx.subscribe(&comment_input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.save(cx);
            }
        });
        Self {
            offer: None,
            editor: None,
            comment_input,
            _comment_input_events: comment_events,
        }
    }

    /// Show the Comment pill for a settled selection. `chat_id` is the chat
    /// selected at settle time (the surface captures it — see
    /// [`CommentPopupEvent::CommentSaved`]). `owner` scopes dismissal;
    /// `head` lets markdown surfaces keep the pill attached to their text
    /// (re-resolved each frame) — terminal selections pass `None` (fixed
    /// window anchor). `clear_selection` drops the source surface's
    /// selection wash on save/cancel/dismiss-and-clear.
    pub fn offer(
        &mut self,
        chat_id: String,
        quote: String,
        anchor: Point<Pixels>,
        owner: CommentOwner,
        head: Option<CommentHead>,
        clear_selection: Rc<dyn Fn(&mut App)>,
        cx: &mut Context<Self>,
    ) {
        self.offer = Some(CommentOffer {
            chat_id,
            quote: normalize_quote(&quote),
            anchor,
            owner,
            head,
            clear_selection,
        });
        self.editor = None;
        cx.notify();
    }

    /// The active affordance's head (editor takes precedence over the pill),
    /// for liveness checks — the row it anchors to may be replaced by a doc
    /// commit, and that invalidation must keep working after the editor
    /// opens.
    pub fn head(&self) -> Option<&CommentHead> {
        self.editor
            .as_ref()
            .and_then(|e| e.head.as_ref())
            .or_else(|| self.offer.as_ref().and_then(|o| o.head.as_ref()))
    }

    /// The transcript/diff row the active offer anchors to, when it is a
    /// markdown offer — the surface checks it is still live each render.
    pub fn offer_row(&self) -> Option<&str> {
        self.head().map(|head| selection::row_of_key(&head.key))
    }

    /// Whether a pill or editor is showing.
    pub fn is_active(&self) -> bool {
        self.offer.is_some() || self.editor.is_some()
    }

    fn active_owner(&self) -> Option<CommentOwner> {
        self.editor
            .as_ref()
            .map(|editor| editor.owner)
            .or_else(|| self.offer.as_ref().map(|offer| offer.owner))
    }

    /// A new selection gesture has started. When it comes from the same
    /// surface, close the old UI without running its cleanup callback because
    /// that callback could clear the selection that was just installed.
    /// Across surfaces, clear the old source selection as well so terminal and
    /// custom-text washes cannot remain visible simultaneously.
    pub fn selection_started(&mut self, owner: CommentOwner, cx: &mut Context<Self>) {
        if self.active_owner().is_some_and(|active| active != owner) {
            self.dismiss_and_clear(cx);
        } else {
            self.dismiss(cx);
        }
    }

    /// Hide the pill/editor without touching the source selection (a fresh
    /// drag / cleared selection calls this).
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.offer.is_some() || self.editor.is_some() {
            self.offer = None;
            self.editor = None;
            cx.notify();
        }
    }

    /// Hide the pill/editor ONLY when it belongs to `owner` (a diff pane
    /// reloading, or terminal output, drops its own comment — never another
    /// surface's). Notifies when something was dismissed.
    pub fn dismiss_if_owner(&mut self, owner: CommentOwner, cx: &mut Context<Self>) {
        let belongs = self
            .editor
            .as_ref()
            .map(|e| e.owner)
            .or_else(|| self.offer.as_ref().map(|o| o.owner))
            == Some(owner);
        if belongs {
            self.offer = None;
            self.editor = None;
            cx.notify();
        }
    }

    /// Hide ONLY the offer pill when it belongs to `owner`, preserving an
    /// open editor and its draft (ongoing terminal output may stale the
    /// quoted endpoint, but must never destroy a comment being typed).
    pub fn dismiss_offer_if_owner(&mut self, owner: CommentOwner, cx: &mut Context<Self>) {
        if self.offer.as_ref().is_some_and(|o| o.owner == owner) {
            self.offer = None;
            cx.notify();
        }
    }

    /// Hide the pill/editor and drop the source selection wash (scroll, chat
    /// switch, row replacement, save, cancel).
    pub fn dismiss_and_clear(&mut self, cx: &mut Context<Self>) {
        let clear = self
            .editor
            .as_ref()
            .map(|e| e.clear_selection.clone())
            .or_else(|| self.offer.as_ref().map(|o| o.clear_selection.clone()));
        self.offer = None;
        self.editor = None;
        if let Some(clear) = clear {
            clear(cx);
        }
        cx.notify();
    }

    /// Turn the pill into the anchored editor (quote preview + input).
    fn open_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(offer) = self.offer.take() else {
            return;
        };
        if offer.quote.is_empty() {
            return;
        }
        self.editor = Some(CommentEditor {
            chat_id: offer.chat_id,
            quote: offer.quote,
            anchor: offer.anchor,
            owner: offer.owner,
            head: offer.head,
            clear_selection: offer.clear_selection,
        });
        self.comment_input
            .update(cx, |input, cx| input.set_text("", cx));
        use gpui::Focusable as _;
        window.focus(&self.comment_input.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    /// Save the open editor: emits [`CommentPopupEvent::CommentSaved`] and
    /// closes. A blank body is a no-op (the editor stays open).
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        let comment = self.comment_input.read(cx).text().trim().to_string();
        if comment.is_empty() {
            self.editor = Some(editor);
            return;
        }
        self.comment_input
            .update(cx, |input, cx| input.set_text("", cx));
        cx.emit(CommentPopupEvent::CommentSaved {
            chat_id: editor.chat_id,
            quote: editor.quote,
            comment,
        });
        self.offer = None;
        (editor.clear_selection)(cx);
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.take() {
            self.comment_input
                .update(cx, |input, cx| input.set_text("", cx));
            self.offer = None;
            (editor.clear_selection)(cx);
            cx.notify();
        }
    }

    /// One-line quote preview for the editor card.
    fn quote_preview(quote: &str) -> String {
        let single = quote.replace('\n', " ");
        if single.chars().count() > 120 {
            let mut out: String = single.chars().take(120).collect();
            out.push('…');
            out
        } else {
            single
        }
    }

    /// The current anchor, re-resolved against a markdown registry when the
    /// offer carries a head (falls back to the mouse-up position).
    fn resolved_anchor(head: &Option<CommentHead>, fallback: Point<Pixels>) -> Point<Pixels> {
        match head {
            Some(head) => {
                render::selection_anchor(head.scope, &head.key, head.ix).unwrap_or(fallback)
            }
            None => fallback,
        }
    }

    /// The floating pill / editor, deferred so it paints above the surfaces
    /// and snaps inside the window. The editor takes precedence over the
    /// pill. Returns `None` when nothing is showing.
    pub fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(editor) = self.editor.clone() {
            return Some(self.render_editor(editor, window, cx));
        }
        let offer = self.offer.clone()?;
        let theme = Theme::of(cx).clone();
        let anchor = Self::resolved_anchor(&offer.head, offer.anchor);
        let weak = cx.weak_entity();
        let pill = div()
            .id("comment-pill")
            .w_auto()
            .flex_none()
            .h(px(24.0))
            .px(px(9.0))
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow_md()
            .occlude()
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .child(SharedString::from("Comment"))
            // A click here must NOT land in the text-selection listener of the
            // surface underneath (its mouse-down would dismiss this very pill).
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                weak.update(cx, |this, cx| this.open_editor(window, cx))
                    .ok();
            });
        Some(
            gpui::deferred(
                gpui::anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .position(anchor + point(px(8.0), px(-10.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(pill),
            )
            .into_any_element(),
        )
    }

    /// The anchored comment editor card: normalized quote preview + a
    /// reusable multiline input + Save/Cancel. Enter saves (via the input's
    /// Submit event), Shift+Enter newlines, Escape / outside click cancels.
    fn render_editor(
        &mut self,
        editor: CommentEditor,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let anchor = Self::resolved_anchor(&editor.head, editor.anchor);
        let has_text = !self.comment_input.read(cx).text().trim().is_empty();
        let save = div()
            .id("comment-save")
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(8.0))
            .bg(theme.text)
            .text_size(px(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(if has_text {
                theme.on_solid
            } else {
                theme.text_faint
            })
            .cursor_pointer()
            .child(SharedString::from("Save"))
            .when(has_text, |el| {
                el.on_click(cx.listener(|this, _, _, cx| this.save(cx)))
            });
        let cancel = div()
            .id("comment-cancel")
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(8.0))
            .text_size(px(11.0))
            .text_color(crate::motion::hover_blend(
                "comment-cancel",
                theme.text_muted,
                theme.text,
            ))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
            .child(SharedString::from("Cancel"));
        let mut cancel_btn = cancel;
        cancel_btn
            .interactivity()
            .on_hover(crate::motion::hover_listener("comment-cancel"));
        let card = div()
            .id("comment-editor")
            .w(px(320.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_overlay)
            .shadow_lg()
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .occlude()
            .child(
                div()
                    .max_h(px(64.0))
                    .overflow_hidden()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(crate::theme::ink(0.03))
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(Self::quote_preview(&editor.quote))),
            )
            .child(self.comment_input.clone())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .items_center()
                    .gap(px(6.0))
                    .child(cancel_btn)
                    .child(save),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.cancel(cx)))
            // Keep the editor alive while interacting with it: without this,
            // the mouse-down on the input/buttons would reach the text-
            // selection listener of the surface underneath and dismiss us.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.cancel(cx);
                    cx.stop_propagation();
                }
            }));
        gpui::deferred(
            gpui::anchored()
                .anchor(gpui::Anchor::TopLeft)
                .position(anchor + point(px(8.0), px(-8.0)))
                .snap_to_window_with_margin(px(8.0))
                .child(card),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_normalization_trims_and_folds_nbsp() {
        assert_eq!(normalize_quote("  plain  "), "plain");
        assert_eq!(
            normalize_quote("\n  leading and trailing  \n"),
            "leading and trailing"
        );
        // NBSP bearings fold to plain spaces (never a literal NBSP quote).
        assert_eq!(normalize_quote("a\u{a0}b"), "a b");
        assert_eq!(normalize_quote("\u{a0}\u{a0}edge\u{a0}"), "edge");
        assert_eq!(normalize_quote("\t tabbed "), "tabbed");
        assert_eq!(normalize_quote(""), "");
    }

    #[test]
    fn comment_editor_quote_preview_collapses_newlines() {
        assert_eq!(CommentPopup::quote_preview("one\ntwo"), "one two");
        assert_eq!(CommentPopup::quote_preview("a\u{a0}b"), "a\u{a0}b");
    }

    /// Each TerminalPanel allocates its own owner id — the drawer panel and
    /// the embedded right-pane panel can never dismiss each other's pill.
    #[test]
    fn next_terminal_allocates_unique_owners() {
        let a = CommentOwner::next_terminal();
        let b = CommentOwner::next_terminal();
        assert!(matches!(a, CommentOwner::Terminal(_)));
        assert_ne!(a, b);
        // A markdown owner never matches a terminal owner (scoped dismissal).
        assert_ne!(
            a,
            CommentOwner::Markdown(selection::SelectionScope::Transcript)
        );
    }

    /// The head-less (terminal) offer keeps its fixed window anchor; a
    /// markdown head falls back to it when the registry has no entry.
    #[test]
    fn resolved_anchor_prefers_head_then_falls_back() {
        let fixed = point(px(100.0), px(200.0));
        // No head: the fixed anchor is returned unchanged.
        assert_eq!(
            CommentPopup::resolved_anchor(&None, fixed),
            point(px(100.0), px(200.0))
        );
        // A head with an unregistered key falls back to the fixed anchor
        // (no gpui App here to build a real registry).
        let head = Some(CommentHead {
            key: "row-t0".into(),
            ix: 4,
            scope: SelectionScope::Transcript,
        });
        assert_eq!(CommentPopup::resolved_anchor(&head, fixed), fixed);
    }
}
