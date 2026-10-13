//! The focused project's floating card plate. One plate paints under the
//! sidebar's card list instead of a fill on each card, so a focus move glides
//! it — position and size — from where it sat onto the card it lands on.
//!
//! Each card records its bounds at prepaint ([`CardPlate::probe`]); the
//! plate is the list's first child, so its paint runs after every card's
//! prepaint in the same frame and always matches the cards' painted layout
//! (resort glides and collapses included).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, Corners, IntoElement, Pixels, Styled, canvas, fill, point, px, size,
};

use crate::kit::motion;
use crate::kit::theme::Theme;

/// The plate's corner radius — the cards' own 12px.
const RADIUS: f32 = 12.0;

#[derive(Clone, Default)]
pub(in crate::shell) struct CardPlate(Rc<RefCell<PlateState>>);

#[derive(Default)]
struct PlateState {
    /// Every rendered card's bounds this frame, keyed by card key.
    cards: HashMap<String, Bounds<Pixels>>,
    /// The plate as last painted — relative to the list's origin, so a scroll
    /// between frames carries it — and its opacity.
    painted: Option<(Bounds<Pixels>, f32)>,
    /// Where the current glide started (a snapshot of `painted`).
    from: Option<(Bounds<Pixels>, f32)>,
}

impl CardPlate {
    /// A new sidebar render: forget the last one's cards, which its prepaint
    /// is about to record again.
    pub(in crate::shell) fn begin_render(&self) {
        self.0.borrow_mut().cards.clear();
    }

    /// Focus moved: the glide starts wherever the plate is now, so a move
    /// interrupting another carries on from mid-flight instead of jumping.
    pub(in crate::shell) fn begin_glide(&self) {
        let mut state = self.0.borrow_mut();
        state.from = state.painted;
    }

    /// Records a card's bounds; place it filling the (relative) card.
    pub(in crate::shell) fn probe(&self, key: String) -> AnyElement {
        let plate = self.clone();
        canvas(
            move |bounds, _, _| {
                plate.0.borrow_mut().cards.insert(key, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    /// The plate itself: the list's first child, filling it. `t` is the
    /// glide's eased progress (1 at rest); with nothing to glide from it fades
    /// in on `focused`, and with no focused card it fades out where it was.
    pub(in crate::shell) fn element(
        &self,
        focused: Option<String>,
        t: f32,
        theme: Theme,
    ) -> AnyElement {
        let plate = self.clone();
        canvas(
            |_, _, _| {},
            move |list, _, window, _| {
                let mut state = plate.0.borrow_mut();
                let to = focused
                    .as_ref()
                    .and_then(|key| state.cards.get(key))
                    .map(|b| Bounds::new(b.origin - list.origin, b.size));
                let (rect, alpha) = match (state.from, to) {
                    (Some((from, a)), Some(to)) => {
                        (lerp_bounds(from, to, t), motion::lerp(a, 1.0, t))
                    }
                    (Some((from, a)), None) => (from, a * (1.0 - t)),
                    (None, Some(to)) => (to, t),
                    (None, None) => (Bounds::default(), 0.0),
                };
                if alpha <= 0.0 {
                    state.painted = None;
                    return;
                }
                state.painted = Some((rect, alpha));
                let bounds = Bounds::new(list.origin + rect.origin, rect.size);
                let radii = Corners::all(px(RADIUS));
                let shadows = theme.sidebar_card_shadows_at(alpha);
                window.paint_drop_shadows(bounds, radii, &shadows);
                window.paint_quad(fill(bounds, theme.sidebar_card_fill(alpha)).corner_radii(radii));
                window.paint_inset_shadows(bounds, radii, &shadows);
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
}

fn lerp_bounds(from: Bounds<Pixels>, to: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let l = |a: Pixels, b: Pixels| px(motion::lerp(f32::from(a), f32::from(b), t));
    Bounds::new(
        point(l(from.origin.x, to.origin.x), l(from.origin.y, to.origin.y)),
        size(
            l(from.size.width, to.size.width),
            l(from.size.height, to.size.height),
        ),
    )
}
