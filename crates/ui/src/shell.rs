//! The app shell (zeron `__root.tsx`): the sidebar column + the workspace of
//! session tiles (docs/workspace-layout.md), plus the boot splash and the
//! connection gate.
//!
//! Layout: collapsible drag-resizable sidebar (208–400px, default 256) with a
//! 200ms ease-out width transition; the workspace column tiles sessions
//! (`shell/workspace_view.rs`), each tile showing one session's chat with its
//! terminal dock and right dock (`shell/session.rs`, `shell/dock.rs`).
//! Widths/collapsed state persist to `ui-settings.json` (debounced).
//!
//! Resize handles use gpui's drag-and-drop pattern (an `on_drag` with an empty
//! ghost view + `on_drag_move::<Marker>` on the root), the same idiom as Zed's
//! dock. Double-clicking a handle resets that pane to its default size.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use gpui::{
    AnyElement, App, Context, Empty, Entity, Focusable as _, IntoElement, KeyBinding, Keystroke,
    MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, Render, SharedString, Subscription,
    Task, Window, WindowControlArea, actions, div, prelude::*, px,
};

use cypher_engine::InstanceLock;
use cypher_proto::{AuthState, ChatIndicator, WorkspaceScope};
use cypher_rpc::methods;
use gpui_tokio::Tokio;

use crate::changes::{Changes, ChangesEvent};
use crate::composer::{Composer, ComposerEvent, ComposerInput, ComposerInputEvent};
use crate::files::FilesPanel;
use crate::kit::icons::{self, cypher_app_icon, icon};
use crate::kit::loaders;
use crate::kit::motion::{self, AnimationExt as _, MotionSpec, RESIZE, SPLASH_OUT, TAB_SLIDE};
use crate::kit::popover::{self, Loadable};
use crate::kit::theme::Theme;
use crate::pickers::AddSpacePalette;
use crate::prefs::slash_commands::ProviderIntent;
use crate::prefs::{
    KeymapConfig, RIGHT_PANE_DEFAULT, RIGHT_PANE_MAX, SAVE_DEBOUNCE_MS, SIDEBAR_DEFAULT,
    SIDEBAR_MAX, SIDEBAR_MIN, TERMINAL_DEFAULT_HEIGHT, UiSettings, platform_combo,
};
use crate::settings::appearance::AppearancePage;
use crate::settings::archived::ArchivedPage;
use crate::settings::commands::{CommandsEvent, CommandsPage};
use crate::settings::device_target::DeviceTarget;
use crate::settings::devices::DevicesPage;
use crate::settings::harnesses::HarnessesPage;
use crate::settings::mcp::McpPage;
use crate::settings::notifications::{NotificationsEvent, NotificationsPage};
use crate::settings::providers::ProvidersPage;
use crate::settings::setup::{SetupEvent, SetupPage};
use crate::settings::shortcuts::{ShortcutsEvent, ShortcutsPage};
use crate::settings::subagents::SubagentsPage;
use crate::state::{
    AppState, ConnectionStatus, EngineBootConfig, EngineMode, GatePhase, Indicator, OrgRow,
    OrgSetup, format_time_ago, org_setup, parse_orgs,
};
use crate::subagents::SubagentsPanel;
use crate::terminal::panel::{TerminalPanel, ToggleTerminal, clamp_terminal_height};
use crate::transcript::rail;
use crate::transcript::{self, Transcript};

#[cfg(feature = "dev-capture")]
mod dev_capture;
mod dock;
pub mod menus;
mod nav;
mod notification_activity;
pub mod notify;
mod org_gate;
mod overlays;
mod render;
use render::*;
mod session;
mod sidebar;
mod spaces;
mod splash;
mod tabs;
mod updates;
mod windows;
mod workspace_view;

use spaces::{AddSpaceFlow, OrphanWorktree, RenameSpaceDialog};

actions!(
    shell,
    [
        ToggleSidebar,
        ToggleChanges,
        FindInChat,
        NewSession,
        NextSession,
        PrevSession,
        SplitRight,
        SplitDown,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        CloseTab,
        ToggleZoom,
        FocusTile1,
        FocusTile2,
        FocusTile3,
        FocusTile4,
        FocusTile5,
        FocusTile6,
        FocusTile7,
        FocusTile8,
        FocusTile9,
        LayoutSingle,
        LayoutColumns2,
        LayoutRows2,
        LayoutColumns3,
        LayoutRows3,
        LayoutGrid2x2,
        LayoutGrid3x3,
        LayoutTwoStackedPlusOne,
        LayoutOnePlusTwoStacked
    ]
);

/// Fixed tile-focus keys: ⌘1…⌘9 (Ctrl elsewhere) focus the Nth tile in
/// reading order. No other binding or key handler claims a primary+digit.
const FOCUS_TILE_KEYS: [&str; 9] = [
    "mod-1", "mod-2", "mod-3", "mod-4", "mod-5", "mod-6", "mod-7", "mod-8", "mod-9",
];

/// The `FocusTile{index + 1}` action.
fn focus_tile_action(index: usize) -> Box<dyn gpui::Action> {
    match index {
        0 => Box::new(FocusTile1),
        1 => Box::new(FocusTile2),
        2 => Box::new(FocusTile3),
        3 => Box::new(FocusTile4),
        4 => Box::new(FocusTile5),
        5 => Box::new(FocusTile6),
        6 => Box::new(FocusTile7),
        7 => Box::new(FocusTile8),
        _ => Box::new(FocusTile9),
    }
}

/// A layout preset's menu label.
pub fn layout_label(preset: crate::workspace::Preset) -> &'static str {
    workspace_view::preset_label(preset)
}

/// The View-menu action applying `preset`.
pub fn layout_action(preset: crate::workspace::Preset) -> Box<dyn gpui::Action> {
    use crate::workspace::Preset;
    match preset {
        Preset::Single => Box::new(LayoutSingle),
        Preset::Columns2 => Box::new(LayoutColumns2),
        Preset::Rows2 => Box::new(LayoutRows2),
        Preset::Columns3 => Box::new(LayoutColumns3),
        Preset::Rows3 => Box::new(LayoutRows3),
        Preset::Grid2x2 => Box::new(LayoutGrid2x2),
        Preset::Grid3x3 => Box::new(LayoutGrid3x3),
        Preset::TwoStackedPlusOne => Box::new(LayoutTwoStackedPlusOne),
        Preset::OnePlusTwoStacked => Box::new(LayoutOnePlusTwoStacked),
    }
}

// ---------------------------------------------------------------------------
// Traffic-light-aware titlebar layout
// ---------------------------------------------------------------------------

/// Where the top-left window-control cluster starts, in px from the window's
/// left edge (zeron window-controls.tsx: `left: fullscreen ? 12 : 88`). The
/// frameless hiddenInset chrome puts the macOS traffic lights at {14,15};
/// fullscreen hides them and the cluster reclaims the inset.
pub fn titlebar_cluster_start(fullscreen: bool) -> f32 {
    if fullscreen { 12.0 } else { 88.0 }
}

/// Width of the persistent top-left button cluster itself (sidebar toggle +
/// back/forward: three 24px buttons, 2px gaps).
pub const CLUSTER_BUTTONS_WIDTH: f32 = 24.0 * 3.0 + 2.0 * 2.0;

const PANEL_EDGE_INSET: f32 = 8.0;
const PANEL_CORNER_RADIUS: f32 = 12.0;
const RIGHT_TAB_RADIUS: f32 = 6.0;
const RIGHT_TAB_HEIGHT: f32 = 24.0;

/// Where the cluster's first button starts, from the window's left edge.
pub fn cluster_buttons_start(is_macos: bool, fullscreen: bool) -> f32 {
    if is_macos {
        titlebar_cluster_start(fullscreen)
    } else {
        10.0
    }
}

/// (Re-)apply the whole app keymap: clears every binding, restores the composer
/// map, then binds the customizable shortcuts from `keymap`. Invalid persisted
/// combos fall back to that shortcut's default.
pub fn apply_keymap(cx: &mut App, keymap: &KeymapConfig) {
    fn valid_or_default(combo: &str, fallback: &str) -> String {
        let candidate = platform_combo(combo);
        if Keystroke::parse(&candidate).is_ok() {
            candidate
        } else {
            tracing::warn!(%combo, "unparseable shortcut combo; using default");
            platform_combo(fallback)
        }
    }
    cx.clear_key_bindings();
    crate::composer::init(cx);
    crate::transcript::init(cx);
    crate::files::editor::init(cx);
    // Fixed app-level shortcuts (⌘Q quit, ⌘W close, ⌘M minimize, ⌘H hide) —
    // these back the native menu key equivalents and must survive keymap
    // re-application.
    crate::shell::menus::bind_keys(cx);
    use crate::prefs::ShortcutId;
    let bind = |id: ShortcutId, action: Box<dyn gpui::Action>| {
        let combo = valid_or_default(keymap.get(id), id.default_combo());
        KeyBinding::load(
            &combo,
            action,
            None,
            false,
            None,
            &gpui::DummyKeyboardMapper,
        )
    };
    let customizable: [(ShortcutId, Box<dyn gpui::Action>); 14] = [
        (ShortcutId::ToggleSidebar, Box::new(ToggleSidebar)),
        (ShortcutId::ToggleChanges, Box::new(ToggleChanges)),
        (ShortcutId::ToggleTerminal, Box::new(ToggleTerminal)),
        (ShortcutId::NewSession, Box::new(NewSession)),
        (ShortcutId::NextSession, Box::new(NextSession)),
        (ShortcutId::PrevSession, Box::new(PrevSession)),
        (ShortcutId::SplitRight, Box::new(SplitRight)),
        (ShortcutId::SplitDown, Box::new(SplitDown)),
        (ShortcutId::FocusLeft, Box::new(FocusLeft)),
        (ShortcutId::FocusRight, Box::new(FocusRight)),
        (ShortcutId::FocusUp, Box::new(FocusUp)),
        (ShortcutId::FocusDown, Box::new(FocusDown)),
        (ShortcutId::CloseTab, Box::new(CloseTab)),
        (ShortcutId::ToggleZoom, Box::new(ToggleZoom)),
    ];
    cx.bind_keys(
        customizable
            .into_iter()
            .filter_map(|(id, action)| bind(id, action).ok()),
    );
    cx.bind_keys([
        // Fixed: ⌘K summons the add-space palette (the ⌘K chip in its search
        // bar); pressing it again dismisses.
        KeyBinding::new(&platform_combo("mod-k"), AddSpacePalette, None),
        // Fixed: ⌘F finds in the open conversation (the universal binding —
        // rebindable shortcuts are the PANEL verbs, and a find bar nobody can
        // guess the key for is a find bar nobody opens).
        KeyBinding::new(&platform_combo("mod-f"), FindInChat, None),
        // Fixed: ⌘, opens Settings (macOS convention).
        KeyBinding::new(
            &platform_combo("mod-,"),
            crate::shell::menus::OpenSettings,
            None,
        ),
    ]);
    // Fixed: ⌘1…⌘9 focus the Nth tile in reading order.
    cx.bind_keys(
        FOCUS_TILE_KEYS
            .iter()
            .enumerate()
            .filter_map(|(index, combo)| {
                KeyBinding::load(
                    &platform_combo(combo),
                    focus_tile_action(index),
                    None,
                    false,
                    None,
                    &gpui::DummyKeyboardMapper,
                )
                .ok()
            }),
    );
}

/// Copy the latest transcript/diff text selection; false when there is none.
fn copy_surface_selection(cx: &mut App) -> bool {
    let Some(text) = crate::markdown::selection::selected_text() else {
        return false;
    };
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
    true
}

/// The settings sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Devices,
    /// Which harnesses the composer offers (enable/disable toggles).
    Harnesses,
    Providers,
    Titles,
    Commands,
    Mcp,
    /// The `agents/*.md` profiles a chat can spawn as children.
    Subagents,
    /// The device's GitHub sign-in (`#` issue references).
    Github,
    Appearance,
    Notifications,
    Shortcuts,
    Archived,
}

impl SettingsSection {
    pub const DEVICE_SETTINGS: [Self; 6] = [
        Self::Harnesses,
        Self::Providers,
        Self::Subagents,
        Self::Commands,
        Self::Mcp,
        Self::Github,
    ];
    pub const CLIENT_SETTINGS: [Self; 3] = [Self::Appearance, Self::Notifications, Self::Shortcuts];
    // Registry and archived conversations span the workspace, not the selected
    // host. Keep them separate from both host configuration and UI preferences.
    pub const WORKSPACE_SETTINGS: [Self; 2] = [Self::Devices, Self::Archived];
    pub const NAV_GROUPS: [(&'static str, &'static [Self]); 3] = [
        ("Device settings", &Self::DEVICE_SETTINGS),
        ("Client settings", &Self::CLIENT_SETTINGS),
        ("Workspace", &Self::WORKSPACE_SETTINGS),
    ];

    #[cfg(test)]
    pub const ALL: [SettingsSection; 11] = [
        SettingsSection::Devices,
        SettingsSection::Harnesses,
        SettingsSection::Providers,
        SettingsSection::Subagents,
        SettingsSection::Commands,
        SettingsSection::Mcp,
        SettingsSection::Github,
        SettingsSection::Appearance,
        SettingsSection::Notifications,
        SettingsSection::Shortcuts,
        SettingsSection::Archived,
    ];

    /// Sidebar + header label (zeron settings-sidebar.tsx SECTIONS / __root.tsx
    /// `settingsTitle` — the same strings in both places).
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::Devices => "Devices",
            SettingsSection::Harnesses => "Agents",
            SettingsSection::Providers => "Providers",
            SettingsSection::Titles => "Automatic titles",
            SettingsSection::Commands => "Commands",
            SettingsSection::Mcp => "MCP",
            SettingsSection::Subagents => "Subagents",
            SettingsSection::Github => "GitHub",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Notifications => "Notifications",
            SettingsSection::Shortcuts => "Shortcuts",
            SettingsSection::Archived => "Archived sessions",
        }
    }
}

/// What the main outlet shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Chat,
    Settings(SettingsSection),
}

/// One route-history entry (zeron parity: the renderer's TanStack memory
/// history — every route the user visited, browser-style).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavEntry {
    /// A chat route; the id of the selected chat ("" = the new-chat canvas).
    Chat(String),
    Settings(SettingsSection),
}

/// Browser-style navigation history for the titlebar back/forward buttons
/// (zeron window-controls.tsx semantics): every route change pushes an entry;
/// Back/Forward walk the stack without changing it; pushing while behind the
/// tip truncates the entries ahead (a new branch, exactly like a browser).
#[derive(Debug)]
pub struct NavHistory {
    entries: Vec<NavEntry>,
    index: usize,
}

impl NavHistory {
    pub fn new(initial: NavEntry) -> Self {
        Self {
            entries: vec![initial],
            index: 0,
        }
    }

    pub fn current(&self) -> &NavEntry {
        &self.entries[self.index]
    }

    /// Record a route change. Re-navigating to the current route is a no-op
    /// (selecting the already-selected chat never happened as a navigation);
    /// otherwise any forward branch is truncated and the entry appended.
    pub fn push(&mut self, entry: NavEntry) {
        if *self.current() == entry {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(entry);
        self.index += 1;
    }

    /// Swap the current entry in place without growing the stack — the native
    /// equivalent of a `replace: true` navigation (zeron's boot redirect from
    /// `/` into the last-used chat leaves no dead Back target behind).
    pub fn replace(&mut self, entry: NavEntry) {
        self.entries[self.index] = entry;
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    /// Memory history keeps every entry, so "behind the last entry" is exactly
    /// "can go forward" (zeron window-controls.tsx).
    pub fn can_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<NavEntry> {
        if !self.can_back() {
            return None;
        }
        self.index -= 1;
        Some(self.current().clone())
    }

    pub fn forward(&mut self) -> Option<NavEntry> {
        if !self.can_forward() {
            return None;
        }
        self.index += 1;
        Some(self.current().clone())
    }

    /// Never zero: the history always holds the route it is currently on, so
    /// there is no empty state for an `is_empty` to report.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Sidebar resort glide: 260ms
/// `cubic-bezier(0.22,1,0.36,1)` per-row translate, the View Transitions
/// equivalent.
pub const RESORT: MotionSpec = MotionSpec::new(260, motion::EASE_RESORT);

/// FLIP diff for a keyed list: given the previously rendered order and the new
/// order (key + row height), return each surviving key's paint-only start
/// offset `old_y - new_y` (only keys whose position actually moved). `gap` is
/// the flex gap between rows. Pure — drives the sidebar resort glide.
pub fn resort_offsets(
    old: &[(String, f32)],
    new: &[(String, f32)],
    gap: f32,
) -> std::collections::HashMap<String, f32> {
    let mut old_y = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in old {
        old_y.insert(key.as_str(), y);
        y += height + gap;
    }
    let mut offsets = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in new {
        if let Some(prev) = old_y.get(key.as_str()) {
            let dy = prev - y;
            if dy.abs() > 0.5 {
                offsets.insert(key.clone(), dy);
            }
        }
        y += height + gap;
    }
    offsets
}

/// Estimated sidebar row height for the resort diff (title line 17px inside
/// 6px vertical padding + the location subline's 14px line + 2px gap — Active
/// rows always carry the folder · device subline).
/// Session row height (FLIP estimate): one inset identity line plus the
/// bottom breathing room between rows.
const CHAT_ROW_HEIGHT: f32 = 30.0;

/// Branch/worktree group header height: deliberately shorter than a session
/// row so checkout sections read as captions, not as list items.
const BRANCH_GROUP_HEADER_HEIGHT: f32 = 24.0;

/// Project card header height: one project + target-machine identity line.
const GROUP_CARD_HEADER_HEIGHT: f32 = 36.0;

/// Bottom padding of a card body with visible rows, so the last session
/// doesn't sit on the card edge.
const GROUP_CARD_BODY_PADDING: f32 = 4.0;
/// Gap between the opaque project cards (the rows inside a card keep a 2px
/// rhythm; the cards themselves breathe like the main/right cards).
const GROUP_CARD_GAP: f32 = 8.0;

/// Ramp height of the sidebar's scroll-edge fade (the gpui
/// [`gpui::EdgeFade`] scope — per-primitive, so text fades per glyph).
const SIDEBAR_GLASS_FADE_BAND: f32 = 32.0;

/// Drag marker for the sidebar resize handle.
struct SidebarResize;

/// Invisible drag ghost — resize drags render nothing at the cursor.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A oneshot width tween (200ms ease-out), driven MANUALLY from render via
/// [`Shell::eval_tween`] — never through a `with_animation` wrapper. gpui keys
/// an animation element's start time by its full global element-id path, so a
/// wrapper that mounts/remounts (route swap, or an ancestor animation keyed by
/// a fresh epoch) silently REPLAYS the tween from t=0. Manual evaluation keeps
/// the element tree's shape constant: a finished or stale tween is exactly the
/// steady state, no matter how the tree around it remounts.
#[derive(Debug, Clone, Copy)]
struct WidthTween {
    from: f32,
    to: f32,
    started: std::time::Instant,
}

impl WidthTween {
    fn new(from: f32, to: f32) -> Self {
        Self {
            from,
            to,
            started: std::time::Instant::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplashPhase {
    Visible,
    FadingOut,
    Gone,
}

/// The chat-row Rename dialog.
struct RenameChatDialog {
    chat_id: String,
    input: Entity<ComposerInput>,
    /// Focus the input on the dialog's first paint (opened without window access).
    focus_pending: bool,
    _events: Subscription,
}

/// In-app update lifecycle (macOS bundle installs; see `render_update_strip`).
enum UpdateFlow {
    Idle,
    Downloading,
    /// Staged bundle ready to swap in — one click restarts into it.
    Ready(PathBuf),
    Failed(SharedString),
}

/// About dialog + Check for Updates status.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AboutCheck {
    Idle,
    Checking,
    Current,
    Available { latest: SharedString },
    Failed { message: SharedString },
}

struct AboutDialog {
    check: AboutCheck,
    /// A manual check sweeps the Pi Runtime too. It runs independently of the
    /// application check because a newer bundle installs itself, which can
    /// take minutes — the dialog must not wait on it.
    runtime_checking: bool,
}

/// Runtime line of the About dialog. The live `PiUpdateStatus` watch (not the
/// manual call's reply) is the source, so a bundle installing in the
/// background reports progress while the dialog is open.
fn about_runtime_line(
    status: Option<&cypher_engine::pi_packages::PiUpdateStatus>,
    checking: bool,
) -> Option<SharedString> {
    let status = status?;
    if status.applying {
        return Some("Updating the Pi Runtime…".into());
    }
    if checking {
        return Some("Checking the Pi Runtime…".into());
    }
    if let Some(error) = status.error.as_deref() {
        return Some(format!("Pi Runtime check failed: {error}").into());
    }
    if !status.pi_installed {
        return None;
    }
    let packages = status.package_updates.len();
    Some(match (status.pi_update_available, packages) {
        (false, 0) => "Pi Runtime is up to date.".into(),
        (true, 0) => match status.latest_pi_version.as_deref() {
            Some(version) => format!("Pi Runtime update available — Pi {version}").into(),
            None => "Pi Runtime update available.".into(),
        },
        (_, 1) => "Pi Runtime update available — 1 plugin.".into(),
        (_, count) => format!("Pi Runtime update available — {count} plugins.").into(),
    })
}

fn install_kind_label(kind: &cypher_update::InstallKind) -> &'static str {
    match kind {
        cypher_update::InstallKind::MacApp { .. } => "macOS app",
        cypher_update::InstallKind::Managed { .. } => "Installed",
        cypher_update::InstallKind::Unmanaged => "Development build",
    }
}

fn about_check_from_status(
    status: Option<&cypher_update::UpdateStatus>,
    app_version: &str,
) -> AboutCheck {
    let Some(status) = status else {
        return AboutCheck::Idle;
    };
    if let Some(error) = status.error.as_deref().filter(|e| !e.is_empty()) {
        return AboutCheck::Failed {
            message: error.to_string().into(),
        };
    }
    let Some(latest) = status.latest_version.as_deref() else {
        return AboutCheck::Idle;
    };
    if cypher_update::version_newer(latest, app_version) {
        AboutCheck::Available {
            latest: latest.to_string().into(),
        }
    } else if status.checked_at.is_some() {
        AboutCheck::Current
    } else {
        AboutCheck::Idle
    }
}

/// Account lifecycle owned by this process. Sign-in on a local workspace
/// flows through the in-place switch wizard (offer → switch → import → done);
/// `RestartPending` survives only as the fallback when the in-place swap
/// fails and a full quit is the safe way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncFlow {
    Idle,
    Enabling,
    Canceling,
    /// Signed in on a local runtime: the wizard's choice step (bring local
    /// work / start fresh / later). `notice_open: false` = postponed, badge
    /// in the account menu.
    SwitchOffer {
        notice_open: bool,
    },
    /// Stopping the local runtime and bootstrapping the synced one in-place.
    Switching {
        import: bool,
    },
    /// The one-time import stream is running on the new synced runtime.
    Importing {
        done: usize,
        total: usize,
    },
    /// Import finished; the success step stays until dismissed.
    ImportDone {
        imported: usize,
        skipped: usize,
    },
    /// The import stream reported errors or died early. Explicit retry step —
    /// structural idempotence makes re-running safe (only missing rows copy).
    /// Details ride `runtime_change_error`. `notice_open: false` = postponed:
    /// the dialog is hidden but the failure stays pending, reachable through
    /// the account menu — dismissal must never discard the only retry
    /// entry point (under Synced scope the menu otherwise offers just
    /// Sign out, and the local rows would be unreachable).
    ImportFailed {
        notice_open: bool,
    },
    RestartPending {
        notice_open: bool,
    },
    SignOutConfirm,
    SigningOut,
    SignedOutRestartRequired,
}

impl SyncFlow {
    /// States the in-place switch driver owns end-to-end — auth/scope edges
    /// must not reset them while the runtime is being replaced under the UI.
    fn is_switch_lifecycle(self) -> bool {
        matches!(
            self,
            SyncFlow::Switching { .. }
                | SyncFlow::Importing { .. }
                | SyncFlow::ImportDone { .. }
                | SyncFlow::ImportFailed { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountMenuAction {
    EnableSync,
    SyncInProgress,
    /// Postponed switch wizard (or legacy restart fallback) — reopen it.
    RestartPending,
    SignOut,
}

const RUNTIME_CHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const RUNTIME_CHANGE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Wait until a stopped daemon can no longer win the next bootstrap probe and
/// has released the data directory for the replacement runtime.
async fn wait_for_remote_engine_shutdown(
    ipc_socket: std::path::PathBuf,
    data_dir: &std::path::Path,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let port_closed = !cypher_rpc::probe_local(&ipc_socket)
            .await
            .map_err(|e| e.to_string())?;
        if port_closed && InstanceLock::holder(data_dir).is_none() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the daemon did not finish stopping within {} seconds",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(RUNTIME_CHANGE_POLL_INTERVAL).await;
    }
}

/// Stop the engine that owns the synced profile and wait until a local runtime
/// can safely acquire both its IPC socket and data-directory lock.
async fn stop_synced_runtime(
    engine: crate::state::EngineHandle,
    ipc_socket: std::path::PathBuf,
    data_dir: &std::path::Path,
) -> Result<(), String> {
    let stop_error = if matches!(engine.mode(), EngineMode::Remote { .. }) {
        engine
            .client()
            .call(methods::STOP_ENGINE, serde_json::json!({}))
            .await
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    engine.shutdown().await;
    match wait_for_remote_engine_shutdown(ipc_socket, data_dir, RUNTIME_CHANGE_TIMEOUT).await {
        Ok(()) => Ok(()),
        Err(error) => match stop_error {
            Some(stop_error) => Err(format!("{stop_error}; {error}")),
            None => Err(error),
        },
    }
}

/// What an import-summary stream item means for the wizard: `Ok((imported,
/// skipped))` only when the engine reported zero errors; otherwise the
/// user-facing failure message. Pure so the partial-failure path is testable.
fn import_summary_outcome(item: &serde_json::Value) -> Result<(usize, usize), String> {
    let count = |key: &str| item.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let errors: Vec<&str> = item
        .get("errors")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if errors.is_empty() {
        return Ok((count("importedChats"), count("skippedChats")));
    }
    let first = errors.first().copied().unwrap_or("unknown error");
    Err(if errors.len() == 1 {
        format!("{} imported, 1 failure: {first}", count("importedChats"))
    } else {
        format!(
            "{} imported, {} failures — first: {first}",
            count("importedChats"),
            errors.len()
        )
    })
}

/// The offer step's description of what a switch would bring along, or `None`
/// when the local profile holds nothing importable. Spaces count as work:
/// a projects-only profile must get the import choice too.
fn local_work_phrase(chats: usize, spaces: usize) -> Option<String> {
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    match (chats, spaces) {
        (0, 0) => None,
        (c, 0) => Some(format!("the {}", plural(c, "session"))),
        (0, s) => Some(format!("the {}", plural(s, "project"))),
        (c, s) => Some(format!(
            "the {} and {}",
            plural(c, "session"),
            plural(s, "project")
        )),
    }
}

fn account_menu_action(scope: Option<WorkspaceScope>, flow: SyncFlow) -> Option<AccountMenuAction> {
    match scope {
        Some(WorkspaceScope::Local) => match flow {
            SyncFlow::Idle => Some(AccountMenuAction::EnableSync),
            SyncFlow::Enabling | SyncFlow::Canceling => Some(AccountMenuAction::SyncInProgress),
            SyncFlow::SwitchOffer { .. } | SyncFlow::RestartPending { .. } => {
                Some(AccountMenuAction::RestartPending)
            }
            SyncFlow::ImportFailed { .. } => Some(AccountMenuAction::RestartPending),
            SyncFlow::Switching { .. }
            | SyncFlow::Importing { .. }
            | SyncFlow::ImportDone { .. } => Some(AccountMenuAction::SyncInProgress),
            SyncFlow::SignOutConfirm
            | SyncFlow::SigningOut
            | SyncFlow::SignedOutRestartRequired => None,
        },
        Some(WorkspaceScope::Synced) => match flow {
            SyncFlow::SignedOutRestartRequired => None,
            // A pending import failure must stay reachable: this is the only
            // surface that can reopen the retry dialog on a synced runtime.
            SyncFlow::ImportFailed { .. } => Some(AccountMenuAction::RestartPending),
            _ if flow.is_switch_lifecycle() => Some(AccountMenuAction::SyncInProgress),
            _ => Some(AccountMenuAction::SignOut),
        },
        Some(WorkspaceScope::Development) | None => None,
    }
}

/// Final UI-side guard before an avatar URL reaches gpui's `img()`: HTTPS
/// only and bounded to 2048 chars. The engine sanitizes every ingress, so
/// this is defense-in-depth — and it also guarantees gpui treats the value as
/// a remote URI, never an embedded file path.
fn safe_avatar_url(url: Option<SharedString>) -> Option<SharedString> {
    let url = url?;
    if url.len() > 2048 {
        return None;
    }
    // The parser percent-encodes whitespace/control chars rather than failing;
    // the raw string must never carry them (a stored avatar URL is fetched
    // verbatim by gpui).
    if url.chars().any(char::is_whitespace) || url.bytes().any(|b| b < 0x20) {
        return None;
    }
    // REAL URL parsing (`url::Url`, the type `reqwest::Url` re-exports): the
    // scheme must be `https`, there must be a host, and there must be no
    // embedded credentials. A hand-rolled prefix check would accept malformed
    // ports/hosts; the parser rejects them — and guarantees gpui treats the
    // value as a remote URI, never an embedded file path. The engine sanitizes
    // every ingress, so this is defense-in-depth.
    let parsed = url::Url::parse(&url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    if parsed.host_str()?.is_empty() || !parsed.username().is_empty() || parsed.password().is_some()
    {
        return None;
    }
    // The WHATWG parser "ignores slashes" right after `https://` —
    // `https:///a.png` silently normalizes to host `a.png` with an EMPTY
    // authority in the input. A real avatar URL names its host explicitly.
    let rest = url.get(url.find(':')? + 1..)?.strip_prefix("//")?;
    if rest.is_empty() || rest.starts_with('/') {
        return None;
    }
    Some(url)
}

fn sync_flow_after_auth(
    flow: SyncFlow,
    scope: Option<WorkspaceScope>,
    auth: Option<&AuthState>,
) -> SyncFlow {
    match scope {
        Some(WorkspaceScope::Local) => match (flow, auth) {
            // The in-place switch owns its own lifecycle once started.
            (flow, _) if flow.is_switch_lifecycle() => flow,
            // AuthStatus belongs to the runtime, not to the Shell that opened
            // the browser. Every attached viewport must advertise the pending
            // profile switch once any of them completes sign-in.
            (SyncFlow::SwitchOffer { .. }, Some(AuthState::SignedOut)) => SyncFlow::Idle,
            (SyncFlow::RestartPending { .. }, Some(AuthState::SignedOut)) => SyncFlow::Idle,
            (SyncFlow::Canceling, Some(AuthState::SignedIn { .. })) => flow,
            (SyncFlow::SwitchOffer { .. }, Some(AuthState::SignedIn { .. })) => flow,
            (SyncFlow::RestartPending { .. }, Some(AuthState::SignedIn { .. })) => flow,
            (_, Some(AuthState::SignedIn { .. })) => SyncFlow::SwitchOffer { notice_open: true },
            _ => flow,
        },
        Some(WorkspaceScope::Synced) => match auth {
            // AuthStatus is shared by every viewport attached to the runtime.
            // Once a synced store loses its credentials, every Shell must stop:
            // letting another viewport sign in would authenticate a new account
            // while the engine still serves the previous account's fixed store.
            Some(AuthState::SignedOut) => SyncFlow::SignedOutRestartRequired,
            _ => match flow {
                SyncFlow::SignOutConfirm
                | SyncFlow::SigningOut
                | SyncFlow::SignedOutRestartRequired => flow,
                flow if flow.is_switch_lifecycle() => flow,
                _ => SyncFlow::Idle,
            },
        },
        Some(WorkspaceScope::Development) => SyncFlow::Idle,
        None => flow,
    }
}

const DEFAULT_PERSONAL_ORG_NAME: &str = "Personal";

/// Organization setup gate. A first personal organization is provisioned
/// automatically; the UI is only interactive when an account belongs to
/// multiple organizations and needs a selection.
struct OrgGateUi {
    orgs: Loadable<Vec<OrgRow>>,
    submitting: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
}

pub struct Shell {
    /// The window's main state: lists (sidebar, spaces, sessions) in
    /// lists-only mode — its `selected_chat` FOLLOWS the focused tile's
    /// session (sidebar highlight, nav history, cycle order, space
    /// implication). Each tile renders from its own session context.
    state: Entity<AppState>,
    /// The tiled session layout (docs/workspace-layout.md).
    workspace: crate::workspace::Workspace,
    /// One slot per open workspace tab (created on open, dropped on close —
    /// dropping the context kills its watches).
    slots: std::collections::HashMap<session::SlotId, session::SessionSlot>,
    next_slot_id: session::SlotId,
    /// The slots on screen at the last workspace change (see
    /// [`Self::dismiss_hidden_comment_ui`]).
    shown_slots: Vec<session::SlotId>,
    /// The focused tab main's selection last followed (see
    /// [`Self::sync_follow`]).
    followed: Option<crate::workspace::TabKey>,
    /// Land keyboard focus in the focused slot's composer on the next
    /// render (routing changes the focused tile without a window handle).
    focus_pending: bool,
    /// Boot landing ran (the first chats frame restored the saved layout or
    /// opened a tab). Layout saves wait for it, so a boot-time save never
    /// overwrites the saved layout with the empty placeholder.
    boot_landed: bool,
    /// The persisted layout to restore at boot landing: the main window's
    /// `UiSettings.workspace`, a project window's
    /// `UiSettings.project_workspaces[project]`. Taken once.
    saved_workspace: Option<crate::workspace::Workspace>,
    /// Unsent drafts + staged attachments of closed session tabs, restored
    /// when the session's slot is created again (drafts used to survive chat
    /// switches). In memory only. Also holds a background fork's prefill
    /// until its tab is first shown.
    closed_drafts:
        std::collections::HashMap<String, (String, Vec<crate::attachments::StagedAttachment>)>,
    /// Terminal panels of closed session tabs, by chat id: their PTYs keep
    /// running (a long job stays reachable) and the panel re-binds to the
    /// session's next slot. Closed for good when the chat is deleted.
    parked_terminals: std::collections::HashMap<String, Entity<TerminalPanel>>,
    /// The main state's chats generation `prune_tabs` last judged: only a
    /// NEW chats frame may close a tab whose chat is missing from the list.
    seen_chats_generation: u64,
    /// Chats this window just created (a fork, a promoted side chat) whose
    /// row may trail a chats frame or two, by creation time: their tabs
    /// aren't closed as deleted until a frame lists them (which clears the
    /// entry) or [`tabs::EXPECTED_CHAT_TTL`] passes.
    expected_chats: std::collections::HashMap<String, std::time::Instant>,
    /// Per tile: its tab strip's scroll handle and the (active tab, tab
    /// count) last scrolled into view — a change scrolls the active tab
    /// fully into view once.
    tile_tab_scroll: std::collections::HashMap<
        crate::workspace::GroupId,
        (
            gpui::ScrollHandle,
            Option<(crate::workspace::TabKey, usize)>,
        ),
    >,
    /// Split containers' measured bounds by tree path (paint-time canvas),
    /// read by the split handles' drag math.
    split_bounds: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<Vec<usize>, gpui::Bounds<Pixels>>>,
    >,
    /// The tile whose session rail last appeared, and a counter bumped each
    /// time focus moves to another tile — keys the rail's entrance
    /// animation so it replays in the newly focused tile.
    rail_focus: (Option<crate::workspace::GroupId>, u64),
    /// Tile bodies' measured bounds by group (paint-time canvas), read by
    /// the session-tab drag's drop-zone hit test.
    tile_bounds: std::rc::Rc<
        std::cell::RefCell<
            std::collections::HashMap<crate::workspace::GroupId, gpui::Bounds<Pixels>>,
        >,
    >,
    /// A live session-tab drag (tile drop catchers mount while `Some`).
    tab_drop: Option<workspace_view::TabDropState>,
    /// The dock surface strip's `+` menu, for the slot that opened it (one
    /// menu is open at a time).
    right_plus: popover::Popup<session::SlotId>,
    /// The titlebar cluster's layout presets popover.
    layout_menu: popover::Popup<()>,
    /// Chat outlet vs settings pages.
    route: Route,
    /// Route history behind the titlebar back/forward buttons (§ nav history).
    nav: NavHistory,
    devices_page: Option<Entity<DevicesPage>>,
    archived_page: Option<Entity<ArchivedPage>>,
    appearance_page: Option<Entity<AppearancePage>>,
    notifications_page: Option<Entity<NotificationsPage>>,
    shortcuts_page: Option<Entity<ShortcutsPage>>,
    providers_page: Option<Entity<ProvidersPage>>,
    titles_page: Option<Entity<crate::settings::titles::TitlesPage>>,
    settings_target: Entity<DeviceTarget>,
    harnesses_page: Option<Entity<HarnessesPage>>,
    commands_page: Option<Entity<CommandsPage>>,
    commands_sub: Option<Subscription>,
    mcp_page: Option<Entity<McpPage>>,
    subagents_page: Option<Entity<SubagentsPage>>,
    github_page: Option<Entity<crate::settings::github::GithubPage>>,
    setup_page: Option<Entity<SetupPage>>,
    setup_sub: Option<Subscription>,
    /// Continue/Skip dismissed the overlay for this process.
    setup_dismissed: bool,
    /// `CYPHER_FORCE_GATE=setup` keeps the first-run overlay visible.
    debug_setup: bool,
    shortcuts_sub: Option<Subscription>,
    notifications_sub: Option<Subscription>,
    /// Session-row context menu: (chat id, window position).
    chat_menu: popover::Popup<(String, Point<Pixels>)>,
    rename_dialog: Option<RenameChatDialog>,
    /// Chat id awaiting delete confirmation.
    delete_confirm: Option<String>,
    /// The engine replaced this app's bundle (a Devices → Update, possibly
    /// from another machine) and armed the relauncher: quit exactly once.
    relaunch_quit_sent: bool,
    /// The quick-chat device palette (sidebar header "Quick chat").
    quick_chat: Option<spaces::QuickChatFlow>,
    /// Scratch-folder removal after a quick chat was deleted (host RPC).
    scratch_cleanup_task: Option<Task<()>>,
    /// Follow-up after the last session of a linked worktree was deleted.
    delete_worktree_confirm: Option<OrphanWorktree>,
    /// Space-row context menu (dropdown rows): (space id, window position).
    space_menu: popover::Popup<(String, Point<Pixels>)>,
    /// Sidebar view menu (device filter + sort): window position.
    sidebar_view_menu: popover::Popup<Point<Pixels>>,
    /// Project glyph/colour picker: (space id, window position).
    space_style_menu: popover::Popup<(String, Point<Pixels>)>,
    rename_space_dialog: Option<RenameSpaceDialog>,
    /// Space id awaiting delete confirmation (hard delete + session cascade).
    delete_space_confirm: Option<String>,
    /// The add-space palette (⌘K-style; device tabs + folder search), `Some`
    /// while open.
    add_space: Option<AddSpaceFlow>,
    /// Scroll position of the sidebar lists region (drives its edge fades).
    sidebar_scroll: gpui::ScrollHandle,
    /// `settings.last_space_id` applied once after the first spaces frame.
    space_boot_applied: bool,
    /// Last seen session status per chat — the chime trigger compares against
    /// it (a row's FIRST appearance never chimes, so boot stays silent).
    sound_prev: std::collections::HashMap<String, cypher_proto::SessionStatus>,
    /// The count last written to the Dock badge (`None` = never written), so
    /// frequent state notifies only touch AppKit when the number changes.
    dock_badge: Option<usize>,
    user_menu: popover::Popup<()>,
    /// Inline sidebar error strip (mutation failures); click dismisses.
    sidebar_notice: Option<SharedString>,
    /// Session Fork idempotence: `(sourceChatId, anchorMessageId) → requestId`
    /// (the client-minted target chat id). The SAME id is reused across RPC
    /// errors / lost replies so a retry returns the already-created chat;
    /// the mapping is dropped on a definitive reply (Created or typed
    /// Unavailable).
    fork_request_ids: std::collections::HashMap<(String, String), String>,
    /// Local lifecycle of an in-app update (macOS bundle swap) — the engine's
    /// UpdateStatus stream says WHETHER one exists; this says how far the
    /// download/stage of it has come in this process.
    update_flow: UpdateFlow,
    update_task: Option<Task<()>>,
    about: Option<AboutDialog>,
    about_task: Option<Task<()>>,
    about_runtime_task: Option<Task<()>>,
    /// Version whose update strip the user dismissed (advisory installs only —
    /// a newer release shows the strip again).
    update_dismissed: Option<String>,
    /// One-click Pi CLI + extension update request. The engine publishes the
    /// checker-side applying/error state; this local bit closes the
    /// click-to-first-watch-frame double-click window.
    pi_update_busy: bool,
    pi_update_task: Option<Task<()>>,
    /// How this binary was installed — decides the strip's click behavior.
    /// Cached: `detect_install` stats `current_exe` and this renders per frame.
    install: cypher_update::InstallKind,
    org: Option<OrgGateUi>,
    sync_flow: SyncFlow,
    mutate_task: Option<Task<()>>,
    delete_worktree_task: Option<Task<()>>,
    auth_task: Option<Task<()>>,
    runtime_change_task: Option<Task<()>>,
    runtime_change_error: Option<SharedString>,
    /// The one-time local→synced import stream (switch wizard progress step).
    import_task: Option<Task<()>>,
    /// Title of the chat the import stream is copying right now.
    import_current: Option<SharedString>,
    /// Kept for the failed-gate "Retry" action.
    boot: EngineBootConfig,
    data_dir: PathBuf,
    settings: UiSettings,
    /// Last rendered sidebar order (key + estimated height) — the FLIP baseline
    /// for the resort glide.
    sidebar_prev_order: Vec<(String, f32)>,
    /// Per-key paint offsets of the resort in flight, keyed elements restart on
    /// `resort_epoch` bumps.
    sidebar_resort: std::collections::HashMap<String, f32>,
    /// Keys that just appeared in a live list (fade in, no glide).
    sidebar_new_keys: std::collections::HashSet<String>,
    resort_epoch: usize,
    /// Collapsed sidebar disclosure groups — local Shell UI state, never
    /// persisted or synced (see `spaces::project_group_key` /
    /// `spaces::branch_group_key` for the deterministic key shapes). A
    /// project key hides the whole card body; a branch/worktree key hides
    /// that group's session rows. Everything defaults expanded.
    sidebar_collapsed: std::collections::HashSet<String>,
    /// Last observed `window.is_window_active()` — rising edge fires a
    /// ProbeSync so a broadcast-deaf room heals as the user looks at the app.
    was_window_active: bool,
    notification_activity: crate::shell::notification_activity::DesktopActivity,
    /// Dev/testing knobs (`CYPHER_OPEN_DIALOG`, `CYPHER_FORCE_GATE`) — see
    /// [`Shell::new`].
    debug_dialog: Option<String>,
    debug_gate: Option<GatePhase>,
    sidebar_tween: Option<WidthTween>,
    /// Last observed `window.is_fullscreen()` (`None` before first paint) —
    /// flips key the traffic-light inset tween.
    fullscreen: Option<bool>,
    /// 200ms ease-out tween of the cluster start on fullscreen toggles.
    titlebar_tween: Option<WidthTween>,
    /// Armed by mouse-down on a titlebar strip; the next mouse-move hands the
    /// drag to the compositor (zed's platform-titlebar pattern).
    titlebar_should_move: bool,
    /// `motion::reduced_motion` snapshot, refreshed at the top of each render
    /// pass so [`Shell::eval_tween`] (called from `&self` render helpers) can
    /// snap without a `cx`.
    reduced_motion: bool,
    /// Set by [`Shell::eval_tween`] when any tween is mid-flight this frame;
    /// render schedules the next animation frame off it.
    motion_active: std::cell::Cell<bool>,
    splash: SplashPhase,
    splash_task: Option<Task<()>>,
    save_task: Option<Task<()>>,
    /// Focus fallback (registered on first paint — [`Shell::new`] has no
    /// window): keyboard shortcuts dispatch through the window focus chain, so
    /// with nothing focused they go dead. Initial focus lands on the composer
    /// and focus lost with no successor routes back there.
    focus_sub: Option<Subscription>,
    /// Keyboard landing spot when the focused tile has no composer (an empty
    /// group): tracked on the root so window shortcuts keep dispatching.
    root_focus: gpui::FocusHandle,
    /// 1s heartbeat re-rendering the working indicator (elapsed + flavour word).
    _ticker: Task<()>,
    _state_observation: Subscription,
    /// Shared floating Comment pill/editor: rendered above every
    /// clipped surface; surfaces (transcript, diff panes, terminals) drive
    /// it through the weak handles they hold.
    comment_popup: Entity<crate::comments::CommentPopup>,
    /// CommentPopup → composer comment forwarding (subscribed ONCE).
    _comment_popup_events: Subscription,
    /// The project a project window is dedicated to; `None` in the main
    /// window (see `shell/windows.rs`).
    project_window: Option<String>,
}

/// Pure presentation of the update strip: given the engine's last
/// UpdateStatus, the version running in THIS process, the version the user
/// dismissed, and this install's flow, decide whether a strip is visible and
/// what it says. Rendered by [`Shell::render_update_strip`]; unit-tested.
struct UpdateStripView {
    label: SharedString,
    clickable: bool,
    /// Failure tone (mac flow): label in danger color, chip tinted red.
    failed: bool,
}

struct PiUpdateStripView {
    label: SharedString,
    clickable: bool,
    failed: bool,
}

fn pi_update_strip_view(
    status: Option<&cypher_engine::pi_packages::PiUpdateStatus>,
    local_busy: bool,
) -> Option<PiUpdateStripView> {
    let status = status?;
    if !status.update_available() {
        return None;
    }
    let package_count = status.package_updates.len();
    let busy = local_busy || status.applying;
    let failed = !busy && status.error.is_some();
    let label = if busy {
        "Updating Pi and plugins…".into()
    } else if failed {
        "Pi/plugin update failed — click to retry".into()
    } else if status.pi_update_available && package_count > 0 {
        format!("Pi and {package_count} plugin updates available").into()
    } else if status.pi_update_available {
        match status.latest_pi_version.as_deref() {
            Some(version) => format!("Pi update available — v{version}").into(),
            None => "Pi update available".into(),
        }
    } else if package_count == 1 {
        "1 plugin update available".into()
    } else {
        format!("{package_count} plugin updates available").into()
    };
    Some(PiUpdateStripView {
        label,
        clickable: !busy,
        failed,
    })
}

fn update_strip_view(
    status: Option<&cypher_update::UpdateStatus>,
    app_version: &str,
    dismissed: Option<&str>,
    mac_app: bool,
    flow: &UpdateFlow,
) -> Option<UpdateStripView> {
    let status = status?;
    let latest = status.latest_version.as_deref()?;
    // The engine may be a DIFFERENT-version daemon than the UI process it
    // serves, so its `update_available` reflects the engine's own version and
    // must not gate this process's strip. Show iff `latest` is newer than the
    // version running here; install kind only changes the action.
    if !cypher_update::version_newer(latest, app_version) {
        return None;
    }
    if dismissed == Some(latest) {
        return None;
    }
    let (label, clickable, failed) = if mac_app {
        match flow {
            UpdateFlow::Idle => (format!("Update available — v{latest}").into(), true, false),
            UpdateFlow::Downloading => (format!("Downloading v{latest}…").into(), false, false),
            UpdateFlow::Ready(_) => ("Update ready — restart to apply".into(), true, false),
            UpdateFlow::Failed(message) => (format!("Update failed: {message}").into(), true, true),
        }
    } else {
        (
            format!("Update available — v{latest} · run `cypher update`").into(),
            true,
            false,
        )
    };
    Some(UpdateStripView {
        label,
        clickable,
        failed,
    })
}

impl Shell {
    /// The main window's shell.
    pub fn new(
        state: Entity<AppState>,
        boot: EngineBootConfig,
        data_dir: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut shell = Self::build(state, boot, data_dir, None, cx);
        shell.register_main_window(cx);
        shell
    }

    /// Shared by the main window and project windows
    /// ([`Shell::new_project_window`]). A project window leaves the app-wide
    /// globals (keymap, hidden slash commands) and the dev capture knobs to
    /// the main window.
    fn build(
        state: Entity<AppState>,
        boot: EngineBootConfig,
        data_dir: PathBuf,
        project_window: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let main_window = project_window.is_none();
        let observation = cx.observe(&state, |this: &mut Shell, state, cx| {
            this.on_state_changed(&state, cx);
            cx.notify();
        });
        // The shared comment popup is created FIRST so every surface can
        // hold a weak handle to it.
        let comment_popup = cx.new(crate::comments::CommentPopup::new);
        if main_window {
            crate::prefs::slash_commands::publish_shown(Vec::new(), cx);
        }
        // Lists-only: the session tiles' contexts own the transcripts; this
        // state's selection just follows the focused tile.
        state.update(cx, |s, cx| s.set_transcript_watches(false, cx));
        // CommentPopup → the owning tile: a comment saved in any surface's
        // anchored editor lands in the pending list (the status-strip
        // indicator) of the tile showing that chat. Subscribed ONCE — the
        // event carries the chat id that was selected when the selection
        // settled (each surface captured it); the composer's guard still
        // drops a comment whose chat is no longer selected.
        let comment_popup_events = cx.subscribe(&comment_popup, {
            move |this: &mut Shell, _, event: &crate::comments::CommentPopupEvent, cx| match event {
                crate::comments::CommentPopupEvent::CommentSaved {
                    chat_id,
                    quote,
                    origin,
                    comment,
                } => {
                    let Some(composer) = this
                        .slot_for_chat(chat_id, cx)
                        .and_then(|sid| this.slots.get(&sid))
                        .map(|slot| slot.composer.clone())
                    else {
                        return;
                    };
                    composer.update(cx, |composer, cx| {
                        composer.add_comment(
                            chat_id.clone(),
                            quote.clone(),
                            origin.clone(),
                            comment.clone(),
                            cx,
                        )
                    });
                }
                crate::comments::CommentPopupEvent::SideChatRequested {
                    chat_id,
                    source,
                    selected_text,
                    origin,
                } => {
                    // Open a temporary Side Chat from the settled
                    // selection (the shell owns the StartSideChat call and
                    // the dock tab). The selected quote rides along so the
                    // engine validates + injects it on the first send.
                    let Some(sid) = this.slot_for_chat(chat_id, cx) else {
                        return;
                    };
                    this.open_side_chat(
                        sid,
                        chat_id.clone(),
                        source.clone(),
                        selected_text.clone(),
                        origin.clone(),
                        cx,
                    );
                }
            }
        });
        // Working-indicator heartbeat: notify once a second while a session is
        // live so elapsed time and the flavour word stay fresh.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |shell: &mut Shell, cx| {
                    // Any visible tile's session counts.
                    let live = {
                        let s = shell.state.read(cx);
                        let now = Utc::now();
                        shell
                            .workspace
                            .visible_tabs()
                            .into_iter()
                            .filter_map(|tab| tab.chat_id())
                            .any(|id| s.indicator_for(id, now) != Indicator::None)
                    };
                    if live
                        || shell
                            .notification_activity
                            .heartbeat_due(std::time::Instant::now())
                    {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let settings_target = cx.new(|cx| DeviceTarget::new(state.clone(), cx));
        let settings = UiSettings::load(&data_dir);
        // A project window's entry here is the file's; the main window's
        // live copy replaces it right after the window opens
        // (`open_project_window`).
        let saved_workspace = match &project_window {
            None => settings.workspace.clone(),
            Some(project) => settings.project_workspaces.get(project).cloned(),
        };
        if main_window {
            crate::prefs::slash_commands::publish_shown(settings.shown_slash_commands.clone(), cx);
            // Bind the customizable shortcuts from the persisted keymap.
            apply_keymap(cx, &settings.keymap);
        }
        // Dev/testing knob: `CYPHER_OPEN_ROUTE=settings[/<section>]` boots
        // straight into a settings section — these pages have no deep link and
        // synthetic input can't reach them on headless compositors.
        let route = match cypher_env::var("OPEN_ROUTE")
            .filter(|_| main_window)
            .as_deref()
        {
            Some("settings") => Route::Settings(SettingsSection::Harnesses),
            Some("settings/devices") => Route::Settings(SettingsSection::Devices),
            Some("settings/providers") => Route::Settings(SettingsSection::Providers),
            Some("settings/titles") => Route::Settings(SettingsSection::Titles),
            Some("settings/harnesses") => Route::Settings(SettingsSection::Harnesses),
            Some("settings/commands") => Route::Settings(SettingsSection::Commands),
            Some("settings/mcp") => Route::Settings(SettingsSection::Mcp),
            Some("settings/subagents") => Route::Settings(SettingsSection::Subagents),
            Some("settings/github") => Route::Settings(SettingsSection::Github),
            Some("settings/appearance") => Route::Settings(SettingsSection::Appearance),
            Some("settings/notifications") => Route::Settings(SettingsSection::Notifications),
            Some("settings/shortcuts") => Route::Settings(SettingsSection::Shortcuts),
            Some("settings/archived") => Route::Settings(SettingsSection::Archived),
            // `new` pins the new-chat canvas (suppresses boot auto-select).
            Some("new") => {
                state.update(cx, |s, _| s.auto_selected = true);
                Route::Chat
            }
            _ => Route::Chat,
        };
        // More capture knobs of the same kind: `CYPHER_OPEN_DIALOG=rename|delete`
        // opens that dialog for the first chat once chats land; `=model` pops
        // the combined harness/model menu once the shell is Ready;
        // `CYPHER_FORCE_GATE=signin|org|failed|setup` renders that gate
        // regardless of real auth state (display-only — for styling passes).
        let debug_dialog = cypher_env::var("OPEN_DIALOG").filter(|_| main_window);
        let force_gate = cypher_env::var("FORCE_GATE").filter(|_| main_window);
        let debug_setup = force_gate.as_deref() == Some("setup");
        let debug_gate = match force_gate.as_deref() {
            Some("signin") => Some(GatePhase::SignIn),
            Some("org") => Some(GatePhase::OrgGate),
            Some("failed") => Some(GatePhase::Failed(
                "Could not reach the cypher engine on port 27901".into(),
            )),
            _ => None,
        };
        let nav = NavHistory::new(match route {
            Route::Chat => NavEntry::Chat(String::new()),
            Route::Settings(section) => NavEntry::Settings(section),
        });
        Self {
            state,
            workspace: crate::workspace::Workspace::new(),
            slots: std::collections::HashMap::new(),
            shown_slots: Vec::new(),
            next_slot_id: 0,
            followed: None,
            focus_pending: false,
            boot_landed: false,
            saved_workspace,
            closed_drafts: std::collections::HashMap::new(),
            parked_terminals: std::collections::HashMap::new(),
            seen_chats_generation: 0,
            expected_chats: std::collections::HashMap::new(),
            tile_tab_scroll: std::collections::HashMap::new(),
            split_bounds: Default::default(),
            rail_focus: (None, 0),
            tile_bounds: Default::default(),
            tab_drop: None,
            right_plus: popover::Popup::default(),
            layout_menu: popover::Popup::default(),
            route,
            nav,
            devices_page: None,
            archived_page: None,
            appearance_page: None,
            notifications_page: None,
            shortcuts_page: None,
            providers_page: None,
            titles_page: None,
            settings_target,
            harnesses_page: None,
            commands_page: None,
            commands_sub: None,
            mcp_page: None,
            subagents_page: None,
            github_page: None,
            setup_page: None,
            setup_sub: None,
            setup_dismissed: false,
            debug_setup,
            shortcuts_sub: None,
            notifications_sub: None,
            chat_menu: popover::Popup::default(),
            rename_dialog: None,
            delete_confirm: None,
            relaunch_quit_sent: false,
            quick_chat: None,
            scratch_cleanup_task: None,
            delete_worktree_confirm: None,
            space_menu: popover::Popup::default(),
            sidebar_view_menu: popover::Popup::default(),
            space_style_menu: popover::Popup::default(),
            rename_space_dialog: None,
            delete_space_confirm: None,
            add_space: None,
            sidebar_scroll: gpui::ScrollHandle::new(),
            space_boot_applied: false,
            sound_prev: std::collections::HashMap::new(),
            dock_badge: None,
            user_menu: popover::Popup::default(),
            sidebar_notice: None,
            fork_request_ids: std::collections::HashMap::new(),
            update_flow: UpdateFlow::Idle,
            update_task: None,
            about: None,
            about_task: None,
            about_runtime_task: None,
            update_dismissed: None,
            pi_update_busy: false,
            pi_update_task: None,
            install: cypher_update::detect_install(),
            org: None,
            sync_flow: SyncFlow::Idle,
            mutate_task: None,
            delete_worktree_task: None,
            auth_task: None,
            runtime_change_task: None,
            runtime_change_error: None,
            import_task: None,
            import_current: None,
            boot,
            data_dir,
            settings,
            sidebar_prev_order: Vec::new(),
            sidebar_resort: std::collections::HashMap::new(),
            sidebar_new_keys: std::collections::HashSet::new(),
            resort_epoch: 0,
            sidebar_collapsed: std::collections::HashSet::new(),
            was_window_active: false,
            notification_activity: Default::default(),
            debug_dialog,
            debug_gate,
            sidebar_tween: None,
            fullscreen: None,
            titlebar_tween: None,
            titlebar_should_move: false,
            reduced_motion: false,
            motion_active: std::cell::Cell::new(false),
            splash: SplashPhase::Visible,
            splash_task: None,
            save_task: None,
            focus_sub: None,
            root_focus: cx.focus_handle(),
            _ticker: ticker,
            _state_observation: observation,
            comment_popup,
            _comment_popup_events: comment_popup_events,
            project_window,
        }
    }

    // ---- layout state ----

    fn sidebar_target(&self) -> f32 {
        if self.settings.sidebar_collapsed {
            0.0
        } else {
            self.settings.sidebar_width
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        let from = self.sidebar_target();
        self.settings.sidebar_collapsed = !self.settings.sidebar_collapsed;
        self.sidebar_tween = Some(WidthTween::new(from, self.sidebar_target()));
        self.schedule_save(cx);
        cx.notify();
    }

    fn on_sidebar_drag(
        &mut self,
        event: &gpui::DragMoveEvent<SidebarResize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let x = f32::from(event.event.position.x);
        self.settings.sidebar_width = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        self.settings.sidebar_collapsed = false;
        self.sidebar_tween = None; // live drag tracks the pointer directly
        self.schedule_save(cx);
        cx.notify();
    }

    /// Debounced settings write: waits [`SAVE_DEBOUNCE_MS`], then persists the
    /// latest snapshot on the background executor. Re-scheduling drops (cancels)
    /// the previous timer.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        // Only the main window writes `ui-settings.json`: a project window's
        // copy is a boot-time snapshot, and saving it whole would clobber
        // whatever the main window changed since. What a project window
        // persists (its layout, session docks) is merged into the main
        // window's copy instead (`Shell::persist_settings`).
        if self.is_project_window() {
            return;
        }
        let dir = self.data_dir.clone();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SAVE_DEBOUNCE_MS))
                .await;
            // Re-stamp the appearance from the global before writing. The View
            // menu changes it through `appearance::set_mode`, which never touches
            // this shell's in-memory copy — without this, the next pane resize
            // would quietly write the boot-time appearance back over the user's
            // choice.
            // The layout is stamped here too (every workspace mutation
            // schedules a save) — once boot restored it, never before.
            let Ok(snapshot) = this.update(cx, |shell, cx| {
                shell.settings.appearance = crate::appearance::mode(cx);
                if shell.boot_landed {
                    shell.settings.workspace = Some(shell.workspace.clone());
                }
                shell.settings.clone()
            }) else {
                return;
            };
            cx.background_executor()
                .spawn(async move {
                    if let Err(err) = snapshot.save(&dir) {
                        tracing::warn!(error = %err, "failed to persist ui settings");
                    }
                })
                .await;
        }));
    }

    fn retry_engine(&mut self, cx: &mut Context<Self>) {
        AppState::bootstrap(
            self.state.clone(),
            self.data_dir.clone(),
            self.boot.clone(),
            cx,
        );
    }
}

#[cfg(test)]
mod tests;
