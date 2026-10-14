//! Live loaders over a cached view.
//!
//! A view embedded with gpui's `cached()` replays its last frame until it is
//! notified, so a loader inside it would either freeze or, leasing the pulse
//! clock for its host, re-render the whole host on every pulse (~30 a
//! second) — for the transcript, rebuilding and laying out every visible row
//! to step a few spinner cells.
//!
//! A [`LiveLayer`] splits the two. The host renders through
//! [`LiveLayer::scope`]: on its real paints, the loaders inside only record
//! their slot (bounds, clip, look). The layer's [`LiveLayer::overlay`],
//! placed right after the cached host by an uncached parent, paints every
//! slot's current frame on every frame and leases the pulse clock for a
//! ticker that nothing observes, so pulses replay the host instead of
//! rebuilding it.
//!
//! The overlay paints above the host's content and below whatever its parent
//! paints after it, each loader under the clip and [`edge_faded`] fade it
//! painted in; an opacity the host applies around a loader is not carried
//! over.
//!
//! [`edge_faded`]: crate::kit::edge_fade::edge_faded
//!
//! Hover fades read inside the host would freeze too, and are pruned when a
//! frame goes by without a read: the scope records the keys the host read,
//! the overlay keeps them alive and re-renders the host while one is
//! mid-fade.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ContentMask, EdgeFade, Element, ElementId, EntityId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Styled, Window, canvas,
};

use crate::kit::edge_fade;
use crate::kit::loaders::Loader;
use crate::kit::motion;

/// The loaders of one cached host view; clones share the slots.
#[derive(Clone, Default)]
pub struct LiveLayer(Rc<RefCell<LayerState>>);

#[derive(Default)]
struct LayerState {
    /// The loaders the host's last real paint placed.
    slots: Vec<Slot>,
    /// The hover-fade keys the host's last render read.
    hover_keys: Vec<String>,
}

/// One loader: where it painted, clipped and faded as its element was.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Slot {
    loader: Loader,
    bounds: Bounds<Pixels>,
    mask: ContentMask<Pixels>,
    fade: Option<EdgeFade>,
}

impl Slot {
    /// Whether any of the loader shows through its clip.
    fn visible(&self) -> bool {
        !self.bounds.intersect(&self.mask.bounds).is_empty()
    }
}

thread_local! {
    /// The layers whose host is painting right now, innermost last.
    static PAINTING: RefCell<Vec<LiveLayer>> = const { RefCell::new(Vec::new()) };
}

impl LiveLayer {
    /// Render a host view's content through the layer: call from the cached
    /// view's `render` with its whole body.
    pub fn scope(&self, render: impl FnOnce() -> AnyElement) -> AnyElement {
        let (child, hover_keys) = motion::capture_hover_reads(render);
        Scope {
            layer: self.clone(),
            child,
            hover_keys,
        }
        .into_any_element()
    }

    /// The layer's painter, to place right after the cached `host` inside
    /// the same clip and fade. Leases the pulse clock for `ticker` while any
    /// recorded loader is visible.
    pub fn overlay(&self, host: EntityId, ticker: EntityId) -> AnyElement {
        let layer = self.clone();
        canvas(
            |_, _, _| (),
            move |_, _, window, cx| layer.paint(host, ticker, window, cx),
        )
        .absolute()
        .size_0()
        .into_any_element()
    }

    /// Whether the host's last paint placed any loader.
    pub fn has_loaders(&self) -> bool {
        !self.0.borrow().slots.is_empty()
    }

    fn paint(&self, host: EntityId, ticker: EntityId, window: &mut Window, cx: &mut App) {
        let state = self.0.borrow();
        motion::keep_hover_alive(&state.hover_keys);
        if motion::hover_in_flight(&state.hover_keys) {
            window.on_next_frame(move |_, cx| cx.notify(host));
        }
        // Loaders scrolled out of view hold no lease, so the clock parks
        // when nothing visible animates.
        for slot in state.slots.iter().filter(|slot| slot.visible()) {
            let delta = motion::pulse_delta(slot.loader.spec(), ticker, cx);
            window.with_edge_fade(slot.fade, |window| {
                window.with_content_mask(Some(slot.mask), |window| {
                    slot.loader.paint(slot.bounds, delta, window, cx);
                });
            });
        }
    }

    fn record(&self, slot: Slot) {
        self.0.borrow_mut().slots.push(slot);
    }
}

/// Paint `loader`'s current frame into `bounds`, leasing the pulse clock for
/// `view` — or, inside a live layer's host, record it for the layer instead.
pub fn paint_loader(
    loader: Loader,
    bounds: Bounds<Pixels>,
    view: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    let layer = PAINTING.with(|painting| painting.borrow().last().cloned());
    if let Some(layer) = layer {
        layer.record(Slot {
            loader,
            bounds,
            mask: window.content_mask(),
            fade: edge_fade::current(),
        });
        return;
    }
    let delta = motion::pulse_delta(loader.spec(), view, cx);
    loader.paint(bounds, delta, window, cx);
}

/// A host's content: laid out and painted as its child, with the layer
/// collecting its loaders and hover reads. It only runs on frames the host
/// renders; replayed frames skip it, keeping the last record.
struct Scope {
    layer: LiveLayer,
    child: AnyElement,
    hover_keys: Vec<String>,
}

impl IntoElement for Scope {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scope {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
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
        // The host rendered: what it places this frame replaces the record.
        {
            let mut state = self.layer.0.borrow_mut();
            state.slots.clear();
            state.hover_keys = std::mem::take(&mut self.hover_keys);
        }
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // Lists build their rows here, after the host's render returned.
        let (_, keys) = motion::capture_hover_reads(|| self.child.prepaint(window, cx));
        self.layer.0.borrow_mut().hover_keys.extend(keys);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        PAINTING.with(|painting| painting.borrow_mut().push(self.layer.clone()));
        let ((), keys) = motion::capture_hover_reads(|| self.child.paint(window, cx));
        PAINTING.with(|painting| painting.borrow_mut().pop());
        self.layer.0.borrow_mut().hover_keys.extend(keys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn slots_outside_their_clip_are_hidden() {
        let slot = |y: f32| Slot {
            loader: Loader::Mini { cell_px: 2.0 },
            bounds: Bounds::new(point(px(10.0), px(y)), size(px(5.0), px(8.0))),
            mask: ContentMask {
                bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(100.0), px(50.0))),
            },
            fade: None,
        };
        assert!(slot(20.0).visible());
        assert!(slot(46.0).visible(), "partly clipped still shows");
        assert!(!slot(60.0).visible(), "scrolled out under the clip");
    }
}
