//! Project windows: a project opened in its own window (the project card's
//! "Open in New Window"). The new window is a second [`Shell`] over a
//! secondary [`AppState`] scoped to that one project
//! ([`AppState::new_project_window`]) — its own selection, transcript and
//! panes, the main window's engine. While it is open the main window hides
//! the project ([`AppState::set_hidden_projects`]); closing it brings the
//! project back. The arrangement is temporary by design: nothing persists,
//! closing the main window closes every project window, and an engine
//! reattach (retry, runtime switch) closes them too — their states hold the
//! old handle.
//!
//! App-wide chrome stays with the main window: Settings, About, the update
//! strips, the user menu, adding projects and quick chats forward there, and
//! only the main window writes `ui-settings.json` and the Dock badge.

use gpui::{Global, WeakEntity, WindowHandle};

use super::*;

/// The window registry: the main shell plus every open project window, keyed
/// by project id.
#[derive(Default)]
struct ProjectWindows {
    main: Option<(WeakEntity<Shell>, WeakEntity<AppState>)>,
    open: Vec<(String, WindowHandle<Shell>)>,
}

impl Global for ProjectWindows {}

fn registry(cx: &mut App) -> &mut ProjectWindows {
    cx.default_global::<ProjectWindows>()
}

/// The main window's shell, when it is alive.
pub(super) fn main_shell(cx: &App) -> Option<Entity<Shell>> {
    cx.try_global::<ProjectWindows>()?
        .main
        .as_ref()?
        .0
        .upgrade()
}

/// Push the set of projects open in their own windows onto the main state.
fn sync_hidden_projects(cx: &mut App) {
    let Some(registry) = cx.try_global::<ProjectWindows>() else {
        return;
    };
    let hidden: std::collections::HashSet<String> =
        registry.open.iter().map(|(id, _)| id.clone()).collect();
    let Some(state) = registry.main.as_ref().and_then(|(_, s)| s.upgrade()) else {
        return;
    };
    state.update(cx, |s, cx| s.set_hidden_projects(hidden, cx));
}

/// Close every project window (deferred: never mid-update of a window).
pub(super) fn close_project_windows(cx: &mut App) {
    let open = std::mem::take(&mut registry(cx).open);
    if open.is_empty() {
        return;
    }
    cx.defer(move |cx| {
        for (_, window) in open {
            window
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
    });
    sync_hidden_projects(cx);
}

impl Shell {
    /// A project window's shell (see the module docs).
    pub fn new_project_window(
        state: Entity<AppState>,
        boot: EngineBootConfig,
        data_dir: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = state.read(cx).window_project().map(str::to_string);
        let mut shell = Self::build(state, boot, data_dir, project.clone(), cx);
        // The state arrives attached and populated — no boot splash, and no
        // first-run setup (the main window owns that flow).
        shell.splash = SplashPhase::Gone;
        shell.setup_dismissed = true;
        shell.space_boot_applied = true;
        cx.on_release(move |_, cx| {
            let Some(project) = project else {
                return;
            };
            registry(cx).open.retain(|(id, _)| *id != project);
            sync_hidden_projects(cx);
        })
        .detach();
        shell
    }

    /// Main window bookkeeping, run once from [`Shell::build`].
    pub(super) fn register_main_window(&mut self, cx: &mut Context<Self>) {
        let entry = (cx.entity().downgrade(), self.state.downgrade());
        registry(cx).main = Some(entry);
        cx.on_release(|_, cx| {
            registry(cx).main = None;
            close_project_windows(cx);
        })
        .detach();
    }

    /// Whether this shell is a project window.
    pub(super) fn is_project_window(&self) -> bool {
        self.project_window.is_some()
    }

    /// The project card's "Open in New Window": focus the project's window
    /// when it is already open, else open one — landing on the selected chat
    /// when it belongs to the project (it moves windows with it).
    pub(super) fn open_project_window(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        if self.is_project_window() {
            return;
        }
        let existing = registry(cx)
            .open
            .iter()
            .find(|(id, _)| *id == space_id)
            .map(|(_, window)| *window);
        if let Some(window) = existing {
            window
                .update(cx, |_, window, _| window.activate_window())
                .ok();
            return;
        }
        let Some(state) = AppState::new_project_window(&self.state, &space_id, cx) else {
            return;
        };
        let Some(window) =
            crate::open_project_window(state, self.boot.clone(), self.data_dir.clone(), cx)
        else {
            self.sidebar_notice = Some("Couldn’t open the project window".into());
            cx.notify();
            return;
        };
        registry(cx).open.push((space_id, window));
        sync_hidden_projects(cx);
        cx.notify();
    }

    /// Project window: close it, returning the project to the main window.
    pub(super) fn close_this_window(&mut self, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let Some(project) = self.project_window.clone() else {
            return;
        };
        let window = registry(cx)
            .open
            .iter()
            .find(|(id, _)| *id == project)
            .map(|(_, window)| *window);
        if let Some(window) = window {
            cx.defer(move |cx| {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
        }
    }

    /// Project window: run `f` on the main window's shell and bring that
    /// window forward — the app-wide chrome (Settings, About, updates, adding
    /// projects) lives there. Deferred so it never runs inside this shell's
    /// own update.
    pub(super) fn forward_to_main(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Shell, &mut Context<Shell>) + 'static,
    ) {
        cx.defer(move |cx| {
            let Some(main) = main_shell(cx) else {
                return;
            };
            main.update(cx, f);
            let window = cx.windows().into_iter().find(|w| {
                w.downcast::<Shell>()
                    .and_then(|w| w.entity(cx).ok())
                    .is_some_and(|shell| shell.entity_id() == main.entity_id())
            });
            if let Some(window) = window {
                window
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
            }
        });
    }

    /// Per-window checks run on every state change:
    /// - main: an engine reattach (connection left Ready) closes the project
    ///   windows — their states hold the old handle;
    /// - project window: the project was removed (here or elsewhere) —
    ///   the window closes itself.
    pub(super) fn sync_window_scope(&mut self, cx: &mut Context<Self>) {
        match &self.project_window {
            None => {
                let ready = matches!(self.state.read(cx).connection, ConnectionStatus::Ready);
                if !ready
                    && cx
                        .try_global::<ProjectWindows>()
                        .is_some_and(|r| !r.open.is_empty())
                {
                    close_project_windows(cx);
                }
            }
            Some(project) => {
                let gone = {
                    let state = self.state.read(cx);
                    state.spaces_synced && state.space_row(project).is_none()
                };
                if gone {
                    self.close_this_window(cx);
                }
            }
        }
    }

    /// The chime / banner settings: the main window's live copy (a project
    /// window's own `settings` is a boot-time snapshot and never saved).
    pub(super) fn chime_settings(&self, cx: &App) -> (bool, bool, bool) {
        let flags = |s: &UiSettings| {
            (
                s.sound_enabled,
                s.notifications_enabled,
                s.notifications_background_only,
            )
        };
        match self.project_window.as_ref().and_then(|_| main_shell(cx)) {
            Some(main) => flags(&main.read(cx).settings),
            None => flags(&self.settings),
        }
    }
}
