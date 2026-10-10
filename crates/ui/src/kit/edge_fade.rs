//! [`edge_faded`] — wraps a child so its whole subtree paints inside a
//! [`gpui::EdgeFade`] scope: primitives fade by vertical distance to the
//! wrapper's own top/bottom edges (per-glyph granularity — a true static
//! gradient, unlike whole-row opacity). Built for the GLASS sidebar's scroll
//! fade: over a see-through blurred backdrop no painted overlay can fade
//! content out, because "what is behind the window" is not a paintable color.

use gpui::{
    AnyElement, App, Bounds, EdgeFade, Element, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, ScrollHandle, Window, px,
};

/// Fade the child's content at its own edges: `top`/`bottom` select which
/// edges (pass the "is there hidden overflow" flags), `band` is the ramp
/// height in px. Horizontal edges via [`EdgeFaded::fade_left`] /
/// [`EdgeFaded::fade_right`].
pub fn edge_faded(band: f32, top: bool, bottom: bool, child: impl IntoElement) -> EdgeFaded {
    EdgeFaded {
        band,
        band_top: None,
        band_bottom: None,
        top,
        bottom,
        left: false,
        right: false,
        scroll_x: None,
        child: child.into_any_element(),
    }
}

pub struct EdgeFaded {
    band: f32,
    band_top: Option<f32>,
    band_bottom: Option<f32>,
    top: bool,
    bottom: bool,
    left: bool,
    right: bool,
    scroll_x: Option<ScrollHandle>,
    child: AnyElement,
}

impl EdgeFaded {
    pub fn fade_left(mut self, on: bool) -> Self {
        self.left = on;
        self
    }

    pub fn fade_right(mut self, on: bool) -> Self {
        self.right = on;
        self
    }

    /// Override the ramp height at the TOP edge only. Asymmetric bands let
    /// content fade across chrome of different heights — a short titlebar
    /// above vs a tall composer stack below.
    pub fn band_top(mut self, px: f32) -> Self {
        self.band_top = Some(px);
        self
    }

    /// [`Self::band_top`], for the bottom edge.
    pub fn band_bottom(mut self, px: f32) -> Self {
        self.band_bottom = Some(px);
        self
    }

    /// Gate [`Self::fade_left`]/[`Self::fade_right`] on the handle's x
    /// overflow, read at PAINT time — after the tracked div's prepaint has
    /// clamped the offset for this frame. Render-time gating rides the LAST
    /// frame's offset, which goes stale on the final frame of a content
    /// shrink: prepaint clamps the offset to fit, nothing re-renders, and a
    /// fade with no overflow sticks on screen (user report). `left`/`right`
    /// become enables; the handle decides per frame (the right-pane
    /// surface-tab strip).
    pub fn fade_overflow_x(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_x = Some(handle.clone());
        self
    }
}

impl Element for EdgeFaded {
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
        let (top, bottom) = (self.top, self.bottom);
        let (mut left, mut right) = (self.left, self.right);
        if let Some(scroll) = &self.scroll_x {
            let scrolled = -f32::from(scroll.offset().x);
            let max_scroll = f32::from(scroll.max_offset().x);
            left &= scrolled > 1.0;
            right &= scrolled < max_scroll - 1.0;
        }
        let fade = (top || bottom || left || right).then(|| EdgeFade {
            bounds,
            band: px(self.band),
            band_top: self.band_top.map(px),
            band_bottom: self.band_bottom.map(px),
            top,
            bottom,
            left,
            right,
        });
        window.with_edge_fade(fade, |window| self.child.paint(window, cx));
    }
}

impl IntoElement for EdgeFaded {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
