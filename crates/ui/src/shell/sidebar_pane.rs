//! The sidebar as its own cached view.
//!
//! The shell is the window's root view, and gpui re-renders the root on every
//! frame. While any session works, the pulse clock drives ~30 frames a second
//! for the loaders, and each of those re-rendered and re-laid-out the whole
//! sidebar — every project card and session row — just to step a few 5×8px
//! working marks. [`SidebarPane`] renders the same shell code behind a cached
//! view boundary instead: gpui replays its last frame until the pane is
//! notified, and the pane follows the shell's own notifications (state
//! changes, hover and tween frames, the heartbeat).
//!
//! Two kinds of motion inside the sidebar would freeze in a replayed frame,
//! so they are carried by the shell, which renders every frame anyway:
//!
//! - Working marks render as fixed-size placeholders that record where they
//!   painted; the shell paints the animated cells over them
//!   ([`SidebarReplay::paint`]), and the pulse clock is leased by
//!   [`PulseTicker`] rather than the shell so its ticks reach nothing else.
//! - Hover fades prune entries no frame read; the keys the last render read
//!   stay alive while its paint is replayed.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, Bounds, ContentMask, Context, EdgeFade, Empty, EntityId, IntoElement,
    ParentElement, Pixels, Point, Render, Styled, Subscription, Task, WeakEntity, Window, canvas,
    div, px,
};

use super::Shell;
use crate::kit::loaders;
use crate::kit::motion::{self, GRADIENT_SPIN};

/// Cell size of the sidebar's working marks.
pub(super) const MARK_CELL: f32 = 2.0;

/// How often a sidebar showing working marks re-renders on its own. A session
/// whose engine stopped heartbeating stops reading as working after a while
/// (`SESSION_STALE_MS`), and nothing notifies when that deadline passes.
const MARK_REFRESH: Duration = Duration::from_secs(1);

/// An invisible view that leases the pulse clock for loaders the shell itself
/// renders. Leasing the shell would notify it on every pulse, and every
/// observer of the shell — the cached sidebar first — would re-render with it;
/// the ticker only needs a frame, and the root re-renders on every frame.
pub(super) struct PulseTicker;

impl Render for PulseTicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// What the cached sidebar needs carried through the frames that replay it:
/// where its working marks sit, and which hover fades its rows read. Written
/// while the sidebar renders and paints, read by the shell on every frame.
#[derive(Clone, Default)]
pub(super) struct SidebarReplay(Rc<RefCell<ReplayState>>);

#[derive(Default)]
struct ReplayState {
    marks: Vec<Mark>,
    /// The session list's top edge fade, which the marks share with the rows
    /// they sit in.
    fade: Option<EdgeFade>,
    hover_keys: Vec<String>,
}

/// One working mark: where its grid painted, clipped as its row is.
struct Mark {
    origin: Point<Pixels>,
    mask: ContentMask<Pixels>,
}

impl SidebarReplay {
    /// A working mark's slot: empty, mark-sized, recording where it painted.
    pub(super) fn mark(&self) -> AnyElement {
        let size = loaders::mini_spinner_size(MARK_CELL);
        let replay = self.clone();
        div()
            .flex_none()
            .w(size.width)
            .h(size.height)
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        replay.0.borrow_mut().marks.push(Mark {
                            origin: bounds.origin,
                            mask: window.content_mask(),
                        });
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }

    /// Records the edge-faded session list's bounds; place it filling the
    /// faded wrapper (`band` is that wrapper's top ramp).
    pub(super) fn fade_probe(&self, band: f32) -> AnyElement {
        let replay = self.clone();
        canvas(
            |_, _, _| (),
            move |bounds, _, _, _| {
                replay.0.borrow_mut().fade = Some(EdgeFade {
                    bounds,
                    band: px(band),
                    band_top: None,
                    band_bottom: None,
                    top: true,
                    bottom: false,
                    left: false,
                    right: false,
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    pub(super) fn has_marks(&self) -> bool {
        !self.0.borrow().marks.is_empty()
    }

    /// Paint the marks' current frame and keep the rows' hover fades alive.
    /// Runs every frame, after the sidebar painted (or replayed).
    pub(super) fn paint(&self, ticker: EntityId, window: &mut Window, cx: &mut gpui::App) {
        let state = self.0.borrow();
        motion::keep_hover_alive(&state.hover_keys);
        let size = loaders::mini_spinner_size(MARK_CELL);
        let visible = |mark: &&Mark| {
            !Bounds::new(mark.origin, size)
                .intersect(&mark.mask.bounds)
                .is_empty()
        };
        // Marks scrolled out of view or under a collapsed sidebar hold no
        // lease, so the clock parks when nothing visible animates.
        if !state.marks.iter().any(|mark| visible(&mark)) {
            return;
        }
        let delta = motion::pulse_delta(&GRADIENT_SPIN, ticker, cx);
        window.with_edge_fade(state.fade, |window| {
            for mark in state.marks.iter().filter(visible) {
                window.with_content_mask(Some(mark.mask), |window| {
                    loaders::paint_mini_spinner(mark.origin, MARK_CELL, delta, window);
                });
            }
        });
    }

    /// A new sidebar render: forget the last one's marks and fade, which its
    /// paint is about to record again.
    fn begin_render(&self) {
        let mut state = self.0.borrow_mut();
        state.marks.clear();
        state.fade = None;
    }

    fn set_hover_keys(&self, keys: Vec<String>) {
        self.0.borrow_mut().hover_keys = keys;
    }
}

/// The sidebar column's content behind a cached view boundary (see the
/// module docs). Rendering still runs the shell's own sidebar code.
pub(super) struct SidebarPane {
    shell: WeakEntity<Shell>,
    observation: Option<Subscription>,
    refresh: Option<Task<()>>,
}

impl SidebarPane {
    pub(super) fn new(shell: WeakEntity<Shell>) -> Self {
        Self {
            shell,
            observation: None,
            refresh: None,
        }
    }

    /// Re-render after [`MARK_REFRESH`] (one timer at a time) — called while
    /// working marks show.
    pub(super) fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_some() {
            return;
        }
        self.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MARK_REFRESH).await;
            this.update(cx, |pane, cx| {
                pane.refresh = None;
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for SidebarPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shell) = self.shell.upgrade() else {
            return Empty.into_any_element();
        };
        // The shell is still being built when the pane is, so the observation
        // starts here: every shell notification re-renders the sidebar, as it
        // did when the shell rendered it inline.
        if self.observation.is_none() {
            self.observation = Some(cx.observe(&shell, |_, _, cx| cx.notify()));
        }
        let replay = shell.read(cx).sidebar.replay.clone();
        replay.begin_render();
        let ((element, tweening), hover_keys) = motion::capture_hover_reads(|| {
            shell.update(cx, |shell, cx| {
                // The shell's tail already ran this frame, so a sidebar tween
                // (the focused card's lift) drives its own frames here.
                let outer = shell.motion.active.replace(false);
                let element = shell.render_sidebar_content(cx);
                (element, shell.motion.active.replace(outer))
            })
        });
        replay.set_hover_keys(hover_keys);
        if tweening {
            window.request_animation_frame();
        }
        element
    }
}
