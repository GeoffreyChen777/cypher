//! [`frosted`] — the frosted-glass float: wraps a popover/dialog card so its
//! ENTIRE subtree paints inside one scene layer (a single draw order) with a
//! backdrop blur painted first.
//!
//! The single layer order is the point: with per-primitive bounds-tree
//! ordering, a hover repaint elsewhere could reassign the card's quads BELOW
//! the blur — washes, dividers, and borders intermittently got snapshotted and
//! blurred away (user reports). Inside one layer the blur/content relationship
//! is structural: blur first, then shadow, tint, border, rows, text.

use gpui::{
    AnyElement, App, Bounds, Corners, Element, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window, point, px, size,
};

use crate::kit::theme::Theme;

/// Backdrop-blur sigma for floating menu/dialog glass — the reference zeron
/// `.glass-surface` runs `blur(44px)`, and the
/// [`Theme::glass_overlay`] tint is thin enough that a 16px blur left
/// backdrop detail ghosting through menu rows. The composer pill keeps its
/// own lighter 16 (`chat-composer-glass` blurs 12–16 in the reference).
pub const MENU_BLUR: f32 = 44.0;

/// The 22px-high Comments/Subagents pills share one frost recipe. Wrapping
/// the existing trigger preserves its hit target and anchored popup; never
/// overflow-clip the trigger, since its inspector opens outside the capsule.
pub fn composer_accessory(child: impl IntoElement) -> Frosted {
    frosted(11.0, 16.0, child)
}

/// Frost `child` (a popover card): backdrop-blurred on glass, pass-through on
/// opaque platforms. `corner_radius` must match the card's rounding.
pub fn frosted(corner_radius: f32, blur_radius: f32, child: impl IntoElement) -> Frosted {
    Frosted {
        corner_radius,
        blur_radius,
        child: child.into_any_element(),
    }
}

pub struct Frosted {
    corner_radius: f32,
    blur_radius: f32,
    child: AnyElement,
}

impl Element for Frosted {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if Theme::of(cx).is_glass() {
            window.paint_layer(bounds, |window| {
                window.paint_backdrop_blur(
                    bounds,
                    Corners::all(px(self.corner_radius)),
                    px(self.blur_radius),
                );
                self.child.paint(window, cx);
            });
        } else {
            self.child.paint(window, cx);
        }
    }
}

impl IntoElement for Frosted {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Paint `child` in its own scene layer, giving it a fresh draw order above
/// everything painted so far in the enclosing layer.
///
/// Needed for overlays INSIDE a frosted card: the card's single layer means
/// every primitive shares one draw order, and equal orders render grouped by
/// primitive kind (quads, then icons, then images) — so a close button's
/// circle painted "after" a thumbnail still shows up UNDER the image. A
/// nested layer restores the intended stacking.
pub fn layered(child: impl IntoElement) -> Layered {
    Layered {
        child: child.into_any_element(),
    }
}

pub struct Layered {
    child: AnyElement,
}

impl Element for Layered {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_layer(bounds, |window| self.child.paint(window, cx));
    }
}

impl IntoElement for Layered {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Bands in a [`frost_pane`]'s blur ramp. The blur pass is uniform per rect,
/// so the ramp is a staircase; ten steps over a titlebar-high pane keep each
/// step's sigma change well under a pixel of perceived blur.
const FROST_PANE_STEPS: usize = 10;

/// A progressive frosted pane — the macOS scroll-edge effect: whatever is
/// painted beneath it blurs, strongest at the pane's top edge and easing
/// linearly to nothing at its bottom, then `child` paints on top — all in one
/// scene layer (see [`frosted`]). No tint and no scroll gating: over an empty
/// backdrop the pane is indistinguishable from the surface around it, and
/// content gains blur gradually as it travels up, never at a hard edge. Pair
/// it with a matching top [`crate::kit::edge_fade`] on the content so rows
/// also fade as they rise. Blurs on every theme (unlike [`frosted`]).
pub fn frost_pane(blur_radius: f32, child: impl IntoElement) -> FrostPane {
    FrostPane {
        blur_radius,
        child: child.into_any_element(),
    }
}

pub struct FrostPane {
    blur_radius: f32,
    child: AnyElement,
}

impl Element for FrostPane {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let step = bounds.size.height / FROST_PANE_STEPS as f32;
        window.paint_layer(bounds, |window| {
            for i in 0..FROST_PANE_STEPS {
                // Sigma at the band's midpoint on the top→bottom ramp.
                let strength = 1.0 - (i as f32 + 0.5) / FROST_PANE_STEPS as f32;
                let band = Bounds::new(
                    point(bounds.origin.x, bounds.origin.y + step * i as f32),
                    size(bounds.size.width, step),
                );
                window.paint_backdrop_blur(
                    band,
                    Corners::default(),
                    px(self.blur_radius * strength),
                );
            }
            self.child.paint(window, cx);
        });
    }
}

impl IntoElement for FrostPane {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// A draw-order barrier: an empty scene layer over the whole window, so
/// everything painted AFTER it draws after everything painted before it.
///
/// gpui orders primitives by bounds overlap, not paint order — a primitive
/// draws right above the highest-ordered earlier primitive it overlaps. A
/// later element that doesn't overlap a backdrop blur (the workspace card
/// beside the sidebar's [`frost_pane`]) can therefore draw BEFORE the blur,
/// and the blur's snapshot (padded ~3σ past its bounds) smears it in — a
/// glow at the pane's edge that stops dead where the pane does (user
/// report). Mount this right after the blurring element and outside any
/// clipping ancestor: the empty layer overlaps everything, so every later
/// primitive orders above the blur and the blur only ever samples what was
/// painted before it.
pub fn order_barrier() -> OrderBarrier {
    OrderBarrier
}

pub struct OrderBarrier;

impl Element for OrderBarrier {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = gpui::Style {
            position: gpui::Position::Absolute,
            ..Default::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let viewport = Bounds::new(point(px(0.0), px(0.0)), window.viewport_size());
        window.paint_layer(viewport, |_| {});
    }
}

impl IntoElement for OrderBarrier {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
