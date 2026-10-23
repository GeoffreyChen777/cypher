//! The sidebar as its own cached view.
//!
//! The shell is the window's root view, and gpui re-renders the root on every
//! frame. While any session works, the pulse clock drives 20 frames a second
//! for the loaders, and each of those re-rendered and re-laid-out the whole
//! sidebar — every project card and session row — just to step a few 5×8px
//! working marks. [`SidebarPane`] renders the same shell code behind a cached
//! view boundary instead: gpui replays its last frame until the pane is
//! notified, and the pane follows the shell's own notifications (state
//! changes, hover and tween frames, the heartbeat).
//!
//! The pane renders through the sidebar's
//! [`LiveLayer`](crate::kit::live_layer::LiveLayer), which the shell paints
//! over it on every frame: the working marks animate and the rows' hover
//! fades stay alive through replayed frames. The pulse clock is leased by
//! [`PulseTicker`] rather than the shell, so its ticks reach nothing else.

use std::time::Duration;

use gpui::{Context, Empty, IntoElement, Render, Subscription, Task, WeakEntity, Window};

use super::Shell;

/// Cell size of the sidebar's working marks.
pub(super) const MARK_CELL: f32 = 2.0;

/// How often a sidebar showing working marks re-renders on its own. A session
/// whose engine stopped heartbeating stops reading as working after a while
/// (`SESSION_STALE_MS`), and nothing notifies when that deadline passes.
const MARK_REFRESH: Duration = Duration::from_secs(1);

/// An invisible view that leases the pulse clock for loaders the shell itself
/// renders, and for the live layers it paints. Leasing the shell would notify
/// it on every pulse, and every observer of the shell — the cached sidebar
/// first — would re-render with it; the ticker only needs a frame, and the
/// root re-renders on every frame.
pub(super) struct PulseTicker;

impl Render for PulseTicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
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
        let live = shell.read(cx).sidebar.live.clone();
        let mut tweening = false;
        let element = live.scope(|| {
            shell.update(cx, |shell, cx| {
                // The shell's tail already ran this frame, so a sidebar tween
                // (the focused card's lift) drives its own frames here.
                let outer = shell.motion.active.replace(false);
                let element = shell.render_sidebar_content(cx);
                tweening = shell.motion.active.replace(outer);
                element
            })
        });
        if tweening {
            window.request_animation_frame();
        }
        element
    }
}
