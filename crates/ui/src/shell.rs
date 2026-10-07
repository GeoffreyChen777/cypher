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
use crate::icons::{self, cypher_app_icon, icon};
use crate::loaders;
use crate::motion::{self, AnimationExt as _, MotionSpec, RESIZE, SPLASH_OUT, TAB_SLIDE};
use crate::popover::{self, Loadable};
use crate::rail;
use crate::settings::accounts::AccountsPage;
use crate::settings::appearance::AppearancePage;
use crate::settings::archived::ArchivedPage;
use crate::settings::commands::{CommandsEvent, CommandsPage};
use crate::settings::device_target::DeviceTarget;
use crate::settings::devices::DevicesPage;
use crate::settings::harnesses::HarnessesPage;
use crate::settings::mcp::McpPage;
use crate::settings::notifications::{NotificationsEvent, NotificationsPage};
use crate::settings::providers::{ProviderIntent, ProvidersPage};
use crate::settings::setup::{SetupEvent, SetupPage};
use crate::settings::shortcuts::{ShortcutsEvent, ShortcutsPage};
use crate::settings::subagents::SubagentsPage;
use crate::settings::{
    KeymapConfig, RIGHT_PANE_DEFAULT, RIGHT_PANE_MAX, SAVE_DEBOUNCE_MS, SIDEBAR_DEFAULT,
    SIDEBAR_MAX, SIDEBAR_MIN, TERMINAL_DEFAULT_HEIGHT, UiSettings, platform_combo,
};
use crate::state::{
    AppState, ConnectionStatus, EngineBootConfig, EngineMode, GatePhase, Indicator, OrgRow,
    OrgSetup, format_time_ago, org_setup, parse_orgs,
};
use crate::subagents::SubagentsPanel;
use crate::terminal::panel::{TerminalPanel, ToggleTerminal, clamp_terminal_height};
use crate::theme::Theme;
use crate::transcript::{self, Transcript};

mod dock;
mod session;
mod spaces;
mod tabs;
mod windows;
mod workspace_view;

use spaces::{AddSpaceFlow, OrphanWorktree, RenameSpaceDialog};

actions!(
    shell,
    [
        ToggleSidebar,
        ToggleChanges,
        AddSpacePalette,
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
    crate::app_menus::bind_keys(cx);
    use crate::settings::ShortcutId;
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
            crate::app_menus::OpenSettings,
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
    /// Per-provider CLI accounts (login, usage) — labeled "Accounts".
    Agents,
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
            SettingsSection::Agents => "Accounts",
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
/// steady state, no matter how the tree around it remounts (round-6 §1–3).
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
    accounts_page: Option<Entity<AccountsPage>>,
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
    /// for the §1.6 resort glide.
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
    notification_activity: crate::notification_activity::DesktopActivity,
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
            crate::settings::commands::publish_shown(Vec::new(), cx);
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
            crate::settings::commands::publish_shown(settings.shown_slash_commands.clone(), cx);
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
            Some("settings/agents") => Route::Settings(SettingsSection::Agents),
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
            accounts_page: None,
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

    // ---- splash ----

    /// Mirror [`AppState::attention_count`] onto the Dock icon (zero when the
    /// setting is off). Written only on change — this runs on every state
    /// notify.
    fn sync_dock_badge(&mut self, cx: &mut Context<Self>) {
        // One Dock icon: the main window owns it (the count is app-wide).
        if self.is_project_window() {
            return;
        }
        let count = if self.settings.dock_badge_enabled {
            self.state.read(cx).attention_count(Utc::now())
        } else {
            0
        };
        if self.dock_badge != Some(count) {
            self.dock_badge = Some(count);
            tracing::debug!(count, "dock badge");
            crate::notify::set_badge(count);
        }
    }

    fn on_state_changed(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        self.sync_window_scope(cx);
        // App-wide flows (relaunch, runtime switches, capture knobs) are the
        // main window's; a project window only renders its project.
        let main_window = !self.is_project_window();
        // A remotely applied update swapped this app's bundle; the relauncher
        // is waiting for this process to exit. Quit through the normal path so
        // the embedded engine flushes before the new bundle opens.
        if main_window
            && !self.relaunch_quit_sent
            && state
                .read(cx)
                .update
                .as_ref()
                .is_some_and(|update| update.relaunch_pending)
        {
            self.relaunch_quit_sent = true;
            tracing::info!(
                "update applied by the engine; quitting so the relauncher can open the new bundle"
            );
            cx.quit();
            return;
        }
        let next_sync_flow = {
            let state = state.read(cx);
            sync_flow_after_auth(self.sync_flow, state.workspace_scope, state.auth.as_ref())
        };
        if main_window && next_sync_flow != self.sync_flow {
            self.sync_flow = next_sync_flow;
            if matches!(
                self.sync_flow,
                SyncFlow::RestartPending { .. } | SyncFlow::SwitchOffer { .. }
            ) {
                self.org = None;
            }
        }
        // The in-place local→synced switch: once the replacement runtime is
        // attached and Ready, kick the import (or finish) from here.
        if main_window {
            self.drive_sync_switch(cx);
        }
        let signed_out_synced = main_window && {
            let state = state.read(cx);
            state.workspace_scope == Some(WorkspaceScope::Synced)
                && matches!(state.auth, Some(AuthState::SignedOut))
        };
        // AuthStatus is shared by every viewport. Whichever viewport owns the
        // embedded runtime drains it; remote viewports request daemon shutdown
        // and all of them independently reattach to the new local runtime.
        if signed_out_synced && self.runtime_change_task.is_none() {
            self.start_local_runtime_transition(false, cx);
        }
        // Capture knob: the add-space palette needs only the device registry.
        if self.debug_dialog.as_deref() == Some("add-space") && !state.read(cx).devices.is_empty() {
            self.debug_dialog = None;
            self.open_add_space(cx);
        }
        // Capture knob: pop the requested dialog once chats have landed.
        if let Some(which) = self.debug_dialog.clone()
            && let Some(first) = state.read(cx).chats.first().map(|c| c.id.clone())
        {
            self.debug_dialog = None;
            match which.as_str() {
                "rename" => self.open_rename_chat(first, cx),
                "delete" => {
                    self.delete_confirm = Some(first);
                }
                _ => {}
            }
        }
        // Session chimes (herdr semantics, `sound::sound_for_transition`): a
        // question rings whenever a session flips to AwaitingInput, a
        // completion rings on the Working→Idle edge — for ANY session on any
        // device. A row's first appearance only seeds the baseline, so boot
        // (restored rows) and fresh sends stay silent. Desktop banners
        // (`notify::post`) ride the SAME edges and gates behind their own
        // settings flag — one detector, two outputs, so the banner can never
        // fire where the chime wouldn't.
        //
        // STALENESS-GATED like the dot (`effective_indicator`), for the same
        // reason: raw row statuses include the past. A dead turn's Working row
        // (host killed mid-run, Idle write lost to a wedged room) seeded
        // prev=Working here, and the moment the old Idle finally synced in —
        // typically piggybacked on the round-trip of a fresh send — the chime
        // heard a phantom Working→Idle and rang "done" on send (user report
        // 2026-07-31). The dot never showed that ghost; the chime must judge
        // by the identical clock.
        //
        // SEND-PENDING-GATED too (`AppState::send_pending`): a send whose
        // queued command the host hasn't executed yet can still surface a
        // phantom Working→Idle (a stale Working row crossing the 45s gate on
        // the send's own re-render, or a late old Idle row) — the done-chime
        // stays quiet for that chat until the host acks, while the baseline
        // keeps tracking silently so the ghost edge never fires later. The
        // question chime is NOT gated: an instant AwaitingInput ack should
        // still ring.
        //
        // WINDOW-SCOPED: each window rings for the sessions it lists, so a
        // project open in its own window rings once, from there. Rows outside
        // the scope still track their baseline silently — a project that
        // returns to the main window never replays an edge it already rang.
        {
            let now = Utc::now();
            type Ping = (
                String,
                cypher_proto::SessionStatus,
                bool,
                Option<String>,
                bool,
            );
            let sessions: Vec<Ping> = {
                let state = state.read(cx);
                let scope = state.project_scope();
                state
                    .sessions
                    .iter()
                    .map(|s| {
                        use cypher_proto::view::Indicator;
                        let status = match cypher_proto::view::effective_indicator(Some(s), now) {
                            Indicator::Working => cypher_proto::SessionStatus::Working,
                            Indicator::AwaitingInput => cypher_proto::SessionStatus::AwaitingInput,
                            Indicator::Errored => cypher_proto::SessionStatus::Errored,
                            Indicator::None => cypher_proto::SessionStatus::Idle,
                        };
                        let send_pending = state.send_pending(&s.chat_id, now);
                        let chat = state.chats.iter().find(|c| c.id == s.chat_id);
                        let title = chat.and_then(|c| c.title.clone());
                        // Rows without a chat yet belong to the main window.
                        let in_scope = chat.map_or(scope.only.is_none(), |c| scope.chat_visible(c));
                        (s.chat_id.clone(), status, send_pending, title, in_scope)
                    })
                    .collect()
            };
            let (sound_enabled, notifications_enabled, background_only) = self.chime_settings(cx);
            // Background-only banners: `active_window()` is app-level (any
            // Cypher window being key), so a ping for a *background chat* in a
            // focused app still stays a chime — you're already looking at
            // Cypher; the sidebar dot carries the rest.
            let app_focused = cx.active_window().is_some();
            for (chat_id, status, send_pending, title, in_scope) in sessions {
                let prev = self.sound_prev.insert(chat_id, status);
                if in_scope
                    && let Some(prev) = prev
                    && let Some(sound) = crate::sound::sound_for_transition(prev, status)
                    && !(send_pending && sound == crate::sound::Sound::Done)
                {
                    if sound_enabled {
                        crate::sound::play(sound);
                    }
                    if notifications_enabled && !(background_only && app_focused) {
                        let title = title.unwrap_or_else(|| "New session".into());
                        let body = match sound {
                            crate::sound::Sound::Done => "Run finished",
                            crate::sound::Sound::Request => "Waiting on your input",
                        };
                        crate::notify::post(&title, body);
                    }
                }
            }
        }
        self.sync_dock_badge(cx);
        // Boot: restore the last selected space once the first spaces frame
        // lands (a still-existing row wins over the auto-selected first one;
        // the boot-auto-selected chat's own space wins over both — selecting a
        // chat implies its space, which `select_chat` already applied).
        if !self.space_boot_applied && !state.read(cx).spaces.is_empty() {
            self.space_boot_applied = true;
            if state.read(cx).selected_chat.is_none() {
                // Restore the last selected project (unless the user opted
                // out of projects); a still-existing row wins over the
                // auto-selected first one. The sidebar never filters — the
                // canvas defaults are the only target.
                let exists = |id: &String| state.read(cx).space_row(id).is_some();
                let target = if !state.read(cx).no_project {
                    self.settings.last_space_id.clone().filter(&exists)
                } else {
                    None
                };
                if target.is_some() {
                    state.update(cx, |s, cx| s.select_space(target.clone(), cx));
                    // A boot canvas tile opened before the spaces frame
                    // copied main's then-empty pick: aim it too.
                    let canvases: Vec<Entity<AppState>> = self
                        .slots
                        .values()
                        .filter(|slot| matches!(slot.tab, crate::workspace::TabKey::NewSession(_)))
                        .map(|slot| slot.state.clone())
                        .collect();
                    for canvas in canvases {
                        canvas.update(cx, |s, cx| s.select_space(target.clone(), cx));
                    }
                }
            }
        }
        // Persist the selected space (the new-tab fallback) — only when it
        // resolves to a LIVE Space. A dangling id (space deleted elsewhere)
        // must never overwrite the remembered one, or the next boot would
        // restore a dead project.
        if main_window {
            let state = state.read(cx);
            let live = state.selected_space_if_live();
            if live.is_some() && live != self.settings.last_space_id {
                self.settings.last_space_id = live;
                self.schedule_save(cx);
            }
        }
        // Boot landing: the most recent session once the first chats frame
        // syncs (manual selection wins).
        self.boot_select_chat(cx);
        // Deleted / archived sessions, and sessions whose project moved to
        // (or out of) this window's scope, leave the workspace.
        self.prune_tabs(cx);
        match state.read(cx).connection {
            ConnectionStatus::Ready => {
                if self.splash == SplashPhase::Visible {
                    self.splash = SplashPhase::FadingOut;
                    self.splash_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(SPLASH_OUT.total() + Duration::from_millis(30))
                            .await;
                        this.update(cx, |shell, cx| {
                            shell.splash = SplashPhase::Gone;
                            cx.notify();
                        })
                        .ok();
                    }));
                }
            }
            // Reveal the gate card immediately; the splash never returns mid-session.
            ConnectionStatus::Failed(_) => self.splash = SplashPhase::Gone,
            ConnectionStatus::Connecting => {}
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

    // ---- routes / settings ----

    /// Close the user menu through the exit animation (no-op when closed).
    fn close_user_menu(&mut self, cx: &mut Context<Self>) {
        if self.user_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.user_menu);
            cx.notify();
        }
    }

    /// Close the session-row context menu through the exit animation.
    fn close_chat_menu(&mut self, cx: &mut Context<Self>) {
        if self.chat_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.chat_menu);
            cx.notify();
        }
    }

    fn dismiss_comment_popup(&mut self, cx: &mut Context<Self>) {
        self.comment_popup
            .update(cx, |popup, cx| popup.dismiss_and_clear(cx));
    }

    fn showing_setup(&self) -> bool {
        crate::settings::setup::setup_should_show(
            self.settings.pi_runtime_setup_version >= 1,
            self.debug_setup,
            self.setup_dismissed,
        )
    }

    fn ensure_setup_page(&mut self, cx: &mut Context<Self>) {
        if self.setup_page.is_some() {
            return;
        }
        let state = self.state.clone();
        let page = cx.new(|cx| SetupPage::new(state, cx));
        self.setup_sub = Some(cx.subscribe(&page, |this, _, event: &SetupEvent, cx| {
            this.complete_setup(cx);
            if matches!(event, SetupEvent::ConfigureProviders) {
                this.open_providers(ProviderIntent::Add, cx);
            }
        }));
        self.setup_page = Some(page);
    }

    fn complete_setup(&mut self, cx: &mut Context<Self>) {
        self.settings.setup_completed = true;
        self.settings.pi_runtime_setup_version = 1;
        self.setup_dismissed = true;
        self.setup_page = None;
        self.setup_sub = None;
        self.schedule_save(cx);
        cx.notify();
    }

    fn render_setup_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_setup_page(cx);
        let theme = Theme::of(cx).clone();
        let inner = match &self.setup_page {
            Some(page) => page.clone().into_any_element(),
            None => Empty.into_any_element(),
        };
        div()
            .id("setup-overlay")
            .absolute()
            .inset_0()
            .size_full()
            .occlude()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .id("setup-overlay-body")
                    .absolute()
                    .inset_0()
                    .size_full()
                    .min_h_0()
                    .child(inner),
            )
            .into_any_element()
    }

    fn open_providers(&mut self, intent: ProviderIntent, cx: &mut Context<Self>) {
        if self.is_project_window() {
            let target = self.settings_target.read(cx).id().map(str::to_string);
            self.forward_to_main(cx, move |main, cx| {
                main.aim_settings_target(target, cx);
                main.open_providers(intent, cx);
            });
            return;
        }
        self.open_settings(SettingsSection::Providers, cx);
        let state = self.state.clone();
        let target = self.settings_target.clone();
        self.providers_page = Some(cx.new(|cx| ProvidersPage::new(state, target, intent, cx)));
    }

    fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        // Settings are app-wide: a project window opens them in the main
        // window, carrying over the device its composer aimed them at.
        if self.is_project_window() {
            let target = self.settings_target.read(cx).id().map(str::to_string);
            self.forward_to_main(cx, move |main, cx| {
                main.aim_settings_target(target, cx);
                main.open_settings(section, cx);
            });
            return;
        }
        if let Some(page) = &self.providers_page {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        if section == SettingsSection::Providers {
            self.providers_page = None;
        }
        if section == SettingsSection::Titles {
            self.titles_page = None;
        }
        // Persisted chat preferences live in the global, not in this editor.
        // Re-enter without a stale font popup or an unfinished HEX draft.
        if section == SettingsSection::Appearance {
            self.appearance_page = None;
        }
        // Recreate per visit: the page's ListHarnesses load re-probes which
        // CLIs are installed, so installing one shows up on the next open.
        if section == SettingsSection::Harnesses {
            self.harnesses_page = None;
        }
        if section == SettingsSection::Commands {
            self.commands_page = None;
        }
        if section == SettingsSection::Mcp {
            self.mcp_page = None;
        }
        // Same reason as the pages above: the profiles are files on the target
        // device, so a fresh visit re-reads them.
        if section == SettingsSection::Subagents {
            self.subagents_page = None;
        }
        // Re-read the sign-in on every visit: a `gh auth login` in a terminal
        // or an expired token should show without restarting.
        if section == SettingsSection::Github {
            self.github_page = None;
        }
        self.dismiss_comment_popup(cx);
        self.route = Route::Settings(section);
        self.nav.push(NavEntry::Settings(section));
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Point the settings device selector at `device` (a forwarded request
    /// from a project window). `None` keeps the current pick.
    fn aim_settings_target(&mut self, device: Option<String>, cx: &mut Context<Self>) {
        if device.is_some() {
            let result = self
                .settings_target
                .update(cx, |target, cx| target.select(device, cx));
            if let Err(error) = result {
                tracing::debug!(%error, "settings target unchanged");
            }
        }
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = &self.providers_page {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        self.route = Route::Chat;
        self.nav.push(NavEntry::Chat(self.focused_chat_key()));
        cx.notify();
    }

    // ---- back/forward (route history) ----

    fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.back() {
            self.apply_nav(entry, cx);
        }
    }

    fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.forward() {
            self.apply_nav(entry, cx);
        }
    }

    /// Land on a history entry WITHOUT recording a new one: the stack already
    /// points at `entry` (back/forward moved the index); the selection change
    /// this triggers dedups against `current()` in [`Self::on_state_changed`].
    fn apply_nav(&mut self, entry: NavEntry, cx: &mut Context<Self>) {
        if let Some(page) = &self.providers_page {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        match entry {
            NavEntry::Chat(chat_id) => {
                self.route = Route::Chat;
                // Land on the entry's tab (reopening it if it was closed);
                // the follow push then dedups against `current()`.
                self.open_nav_target(chat_id, cx);
            }
            NavEntry::Settings(section) => {
                self.dismiss_comment_popup(cx);
                self.route = Route::Settings(section);
            }
        }
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Lazily create the entity for a settings section and return it renderable.
    fn settings_outlet(&mut self, section: SettingsSection, cx: &mut Context<Self>) -> AnyElement {
        match section {
            SettingsSection::Titles => {
                if self.titles_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.titles_page = Some(
                        cx.new(|cx| crate::settings::titles::TitlesPage::new(state, target, cx)),
                    );
                }
                self.titles_page
                    .as_ref()
                    .unwrap()
                    .clone()
                    .into_any_element()
            }
            SettingsSection::Providers => {
                if self.providers_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.providers_page = Some(
                        cx.new(|cx| ProvidersPage::new(state, target, ProviderIntent::List, cx)),
                    );
                }
                self.providers_page
                    .as_ref()
                    .unwrap()
                    .clone()
                    .into_any_element()
            }
            SettingsSection::Devices => {
                if self.devices_page.is_none() {
                    let state = self.state.clone();
                    self.devices_page = Some(cx.new(|cx| DevicesPage::new(state, cx)));
                }
                match &self.devices_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Harnesses => {
                if self.harnesses_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.harnesses_page = Some(cx.new(|cx| HarnessesPage::new(state, target, cx)));
                }
                match &self.harnesses_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Agents => {
                if self.accounts_page.is_none() {
                    let state = self.state.clone();
                    self.accounts_page = Some(cx.new(|cx| AccountsPage::new(state, cx)));
                }
                match &self.accounts_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Commands => {
                if self.commands_page.is_none() {
                    let state = self.state.clone();
                    let shown = self.settings.shown_slash_commands.clone();
                    let target = self.settings_target.clone();
                    let page = cx.new(|cx| CommandsPage::new(state, target, shown, cx));
                    self.commands_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &CommandsEvent, cx| {
                            let CommandsEvent::Changed(shown) = event;
                            this.settings.shown_slash_commands = shown.clone();
                            crate::settings::commands::publish_shown(shown.clone(), cx);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.commands_page = Some(page);
                }
                match &self.commands_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Mcp => {
                if self.mcp_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.mcp_page = Some(cx.new(|cx| McpPage::new(state, target, cx)));
                }
                match &self.mcp_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Subagents => {
                if self.subagents_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.subagents_page = Some(cx.new(|cx| SubagentsPage::new(state, target, cx)));
                }
                match &self.subagents_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Github => {
                if self.github_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.github_page = Some(
                        cx.new(|cx| crate::settings::github::GithubPage::new(state, target, cx)),
                    );
                }
                match &self.github_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Appearance => {
                if self.appearance_page.is_none() {
                    self.appearance_page = Some(cx.new(AppearancePage::new));
                }
                match &self.appearance_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Notifications => {
                if self.notifications_page.is_none() {
                    let page = cx.new(|cx| {
                        NotificationsPage::new(
                            self.settings.sound_enabled,
                            self.settings.notifications_enabled,
                            self.settings.notifications_background_only,
                            self.settings.dock_badge_enabled,
                            cx,
                        )
                    });
                    // Persist the flags whenever the page flips one.
                    self.notifications_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &NotificationsEvent, cx| {
                            let NotificationsEvent::Changed {
                                sound,
                                desktop,
                                background_only,
                                dock_badge,
                            } = *event;
                            this.settings.sound_enabled = sound;
                            this.settings.notifications_enabled = desktop;
                            this.settings.notifications_background_only = background_only;
                            this.settings.dock_badge_enabled = dock_badge;
                            this.sync_dock_badge(cx);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.notifications_page = Some(page);
                }
                match &self.notifications_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Shortcuts => {
                if self.shortcuts_page.is_none() {
                    let state = self.state.clone();
                    let keymap = self.settings.keymap.clone();
                    let page = cx.new(|cx| ShortcutsPage::new(state, keymap, cx));
                    // Persist + re-apply the keymap whenever the page changes it.
                    self.shortcuts_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &ShortcutsEvent, cx| {
                            let ShortcutsEvent::Changed(keymap) = event;
                            this.settings.keymap = keymap.clone();
                            apply_keymap(cx, keymap);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.shortcuts_page = Some(page);
                }
                match &self.shortcuts_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Archived => {
                if self.archived_page.is_none() {
                    let state = self.state.clone();
                    self.archived_page = Some(cx.new(|cx| ArchivedPage::new(state, cx)));
                }
                match &self.archived_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
        }
    }

    // ---- sidebar mutations ----

    /// Fire a Mutate op; failures surface in the sidebar notice strip.
    fn mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(methods::MUTATE, params).await {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn open_rename_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        let current = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .and_then(|c| c.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| ComposerInput::new("Session title", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_chat(cx);
            }
        });
        self.rename_dialog = Some(RenameChatDialog {
            chat_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    fn submit_rename_chat(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_dialog.take() else {
            return;
        };
        let title = dialog.input.read(cx).text().trim().to_string();
        if !title.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameChat", "chatId": dialog.chat_id, "title": title }),
                cx,
            );
        }
        cx.notify();
    }

    fn archive_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.set_chat_archived(chat_id, true, cx);
    }

    pub(super) fn set_chat_archived(
        &mut self,
        chat_id: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatArchived", "chatId": chat_id, "archived": archived }),
            cx,
        );
        cx.notify();
    }

    fn close_sidebar_view_menu(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_view_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.sidebar_view_menu);
            cx.notify();
        }
    }

    /// The sidebar view menu's current state (persisted in ui-settings).
    pub(super) fn sidebar_view(&self) -> crate::state::SidebarView {
        crate::state::SidebarView {
            // A project window lists one project: a device filter could only
            // empty it.
            device: self
                .settings
                .sidebar_device_filter
                .clone()
                .filter(|_| !self.is_project_window()),
            sort: self.settings.sidebar_sort,
            reversed: self.settings.sidebar_sort_reversed,
        }
    }

    /// A new sort starts in its natural direction.
    fn set_sidebar_sort(&mut self, sort: crate::settings::SidebarSort, cx: &mut Context<Self>) {
        if self.settings.sidebar_sort != sort {
            self.settings.sidebar_sort_reversed = false;
        }
        self.settings.sidebar_sort = sort;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn set_sidebar_descending(&mut self, descending: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_sort_reversed =
            self.settings.sidebar_sort.natural_descending() != descending;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn set_sidebar_device_filter(&mut self, device: Option<String>, cx: &mut Context<Self>) {
        self.settings.sidebar_device_filter = device;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Pin/unpin a session (synced): pinned sessions lead their project.
    fn set_chat_pinned(&mut self, chat_id: String, pinned: bool, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatPinned", "chatId": chat_id, "pinned": pinned }),
            cx,
        );
        cx.notify();
    }

    fn delete_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        let orphan = {
            let state = self.state.read(cx);
            spaces::orphan_worktree_after_delete(&state.chats, &state.spaces, &chat_id)
        };
        // A quick chat's scratch folder dies with the row: captured before
        // the mutate while the row is still in the local list.
        let scratch = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id && c.is_scratch())
            .and_then(|c| c.cwd.clone().map(|cwd| (c.device_id.clone(), cwd)));
        // Its tab closes (the slot's composer, and its per-chat stage, go
        // with it); a stashed draft of a closed tab is dropped too.
        if let Some(sid) = self.slot_for_tab(&crate::workspace::TabKey::session(chat_id.clone()))
            && let Some(slot) = self.slots.get(&sid)
        {
            slot.composer
                .update(cx, |composer, _| composer.purge_chat(&chat_id));
        }
        self.close_tab(&crate::workspace::TabKey::session(chat_id.clone()), cx);
        self.closed_drafts.remove(&chat_id);
        self.mutate(
            serde_json::json!({ "op": "deleteChat", "chatId": chat_id.clone() }),
            cx,
        );
        if let Some((device_id, cwd)) = scratch {
            self.delete_scratch_dir(chat_id, device_id, cwd, cx);
        }
        // Last session of a linked worktree: ask whether to remove the
        // checkout too. Captured before the mutate so the row is still in
        // the local list; children of this chat cascade and don't count.
        self.delete_worktree_confirm = orphan;
        cx.notify();
    }

    /// Remove a deleted quick chat's scratch folder on its host. The host
    /// verifies the path is the folder it minted for this chat id before
    /// deleting anything; a failure is surfaced, never retried silently.
    fn delete_scratch_dir(
        &mut self,
        chat_id: String,
        device_id: String,
        cwd: String,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let mut params = serde_json::json!({ "chatId": chat_id, "path": cwd });
        if local.as_deref() != Some(device_id.as_str())
            && let Some(object) = params.as_object_mut()
        {
            object.insert("targetDeviceId".into(), device_id.into());
        }
        self.scratch_cleanup_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine
                .client()
                .call(methods::DELETE_SCRATCH_DIR, params)
                .await
            {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice =
                        Some(format!("Scratch folder not removed: {err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn delete_worktree(&mut self, orphan: OrphanWorktree, cx: &mut Context<Self>) {
        self.delete_worktree_confirm = None;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let mut params = serde_json::Map::new();
        params.insert("repoPath".into(), orphan.repo_path.into());
        params.insert("worktreePath".into(), orphan.worktree_path.into());
        if local.as_deref() != Some(orphan.device_id.as_str()) {
            params.insert("targetDeviceId".into(), orphan.device_id.into());
        }
        self.delete_worktree_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine
                .client()
                .call(methods::DELETE_WORKTREE, serde_json::Value::Object(params))
                .await
            {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn request_sign_out(&mut self, cx: &mut Context<Self>) {
        self.close_user_menu(cx);
        if self.state.read(cx).workspace_scope != Some(WorkspaceScope::Synced) {
            return;
        }
        self.sync_flow = SyncFlow::SignOutConfirm;
        cx.notify();
    }

    fn confirm_sign_out(&mut self, cx: &mut Context<Self>) {
        self.start_local_runtime_transition(true, cx);
    }

    fn start_local_runtime_transition(&mut self, sign_out: bool, cx: &mut Context<Self>) {
        if self.runtime_change_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            self.sync_flow = SyncFlow::SignedOutRestartRequired;
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::SigningOut;
        self.runtime_change_error = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let shutdown_dir = data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            if sign_out {
                engine
                    .client()
                    .call(methods::SIGN_OUT, serde_json::json!({}))
                    .await
                    .map_err(|error| format!("Sign out failed: {error}"))?;
            }
            stop_synced_runtime(engine, ipc_socket, &shutdown_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        shell.sync_flow = SyncFlow::Idle;
                        shell.runtime_change_error = None;
                        shell.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), shell.data_dir.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync_flow = SyncFlow::SignedOutRestartRequired;
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn cancel_auth_setup(&mut self, cx: &mut Context<Self>) {
        let local = self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let pending_auth = self.auth_task.take();
        let pending_org = self.org.as_mut().and_then(|org| org.task.take());
        if local {
            self.sync_flow = SyncFlow::Canceling;
        }
        self.auth_task = Some(cx.spawn(async move |this, cx| {
            // Do not race SignOut against an exchange or organization write
            // that can still persist a session after credentials were cleared.
            if let Some(task) = pending_auth {
                task.await;
            }
            if let Some(task) = pending_org {
                task.await;
            }
            let result = engine
                .client()
                .call(methods::SIGN_OUT, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => {
                        shell.org = None;
                        if local {
                            shell.sync_flow = SyncFlow::Idle;
                        }
                    }
                    Err(err) => {
                        if local {
                            shell.sync_flow = SyncFlow::Enabling;
                        }
                        shell.sidebar_notice =
                            Some(format!("Could not cancel sign-in: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn postpone_sync_restart(&mut self, cx: &mut Context<Self>) {
        match self.sync_flow {
            SyncFlow::RestartPending { .. } => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: false };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync_flow = SyncFlow::SwitchOffer { notice_open: false };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync_flow = SyncFlow::ImportFailed { notice_open: false };
            }
            _ => return,
        }
        cx.notify();
    }

    fn reopen_sync_notice(&mut self, cx: &mut Context<Self>) {
        self.close_user_menu(cx);
        match self.sync_flow {
            SyncFlow::RestartPending { .. } => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync_flow = SyncFlow::SwitchOffer { notice_open: true };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync_flow = SyncFlow::ImportFailed { notice_open: true };
            }
            _ => return,
        }
        cx.notify();
    }

    /// The wizard's choice step chose a path: stop the local runtime, boot the
    /// synced one in-place (mirror of the sign-out transition), then let
    /// [`Self::drive_sync_switch`] run the import once the runtime is ready.
    /// Failure falls back to the quit-and-reopen dialog — the local profile is
    /// untouched, so the old path is always a safe exit.
    fn start_synced_switch(&mut self, import: bool, cx: &mut Context<Self>) {
        if self.runtime_change_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::Switching { import };
        self.runtime_change_error = None;
        self.import_current = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            stop_synced_runtime(engine, ipc_socket, &data_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        // Keep `Switching { import }`: the state observer sees
                        // the replacement runtime reach Ready and advances the
                        // wizard from there.
                        shell.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), shell.data_dir.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync_flow = SyncFlow::RestartPending { notice_open: true };
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Advance the in-place switch when the replacement runtime lands: Ready +
    /// Synced starts the import stream (or finishes immediately when the user
    /// chose a fresh start); a runtime that comes back non-synced fell out of
    /// the swap — surface the quit fallback rather than pretend.
    fn drive_sync_switch(&mut self, cx: &mut Context<Self>) {
        let SyncFlow::Switching { import } = self.sync_flow else {
            return;
        };
        if self.runtime_change_task.is_some() {
            return; // still stopping the local runtime
        }
        let (ready, scope) = {
            let state = self.state.read(cx);
            (
                matches!(state.connection, ConnectionStatus::Ready),
                state.workspace_scope,
            )
        };
        if !ready {
            if let ConnectionStatus::Failed(error) = &self.state.read(cx).connection {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
                self.runtime_change_error = Some(error.clone().into());
                cx.notify();
            }
            return;
        }
        match scope {
            Some(WorkspaceScope::Synced) => {
                if import {
                    self.spawn_local_import(cx);
                } else {
                    self.sync_flow = SyncFlow::Idle;
                    cx.notify();
                }
            }
            Some(_) => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
                self.runtime_change_error =
                    Some("The synced workspace did not come up — restart to finish.".into());
                cx.notify();
            }
            None => {}
        }
    }

    /// Subscribe to the engine's one-time import stream and mirror its
    /// progress into the wizard.
    fn spawn_local_import(&mut self, cx: &mut Context<Self>) {
        if self.import_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            self.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::Importing { done: 0, total: 0 };
        self.runtime_change_error = None;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let stream = Tokio::spawn(cx, async move {
            let mut items = engine
                .client()
                .subscribe(methods::IMPORT_LOCAL_WORKSPACE, serde_json::json!({}))
                .await
                .map_err(|error| error.to_string())?;
            while let Some(item) = items.recv().await {
                let _ = tx.send(item);
            }
            Ok::<(), String>(())
        });
        self.import_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let item = rx.recv().await;
                let ended = item.is_none();
                this.update(cx, |shell, cx| {
                    if let Some(item) = &item {
                        shell.apply_import_event(item, cx);
                    }
                    if ended {
                        shell.import_task = None;
                        shell.import_current = None;
                        // A stream that died before its summary is a failure —
                        // offer the in-place retry (idempotent).
                        if matches!(shell.sync_flow, SyncFlow::Importing { .. }) {
                            shell.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                            shell.runtime_change_error =
                                Some("The import stream ended before it finished.".into());
                        }
                        cx.notify();
                    }
                })
                .ok();
                if ended {
                    break;
                }
            }
            if let Ok(Err(error)) = stream.await {
                this.update(cx, |shell, cx| {
                    shell.import_task = None;
                    if matches!(shell.sync_flow, SyncFlow::Importing { .. }) {
                        shell.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn apply_import_event(&mut self, item: &serde_json::Value, cx: &mut Context<Self>) {
        match item.get("kind").and_then(|k| k.as_str()) {
            Some("start") => {
                let total = item.get("chats").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.sync_flow = SyncFlow::Importing { done: 0, total };
            }
            Some("chat") => {
                let index = item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let total = item.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.import_current = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|t| SharedString::from(t.to_string()));
                self.sync_flow = SyncFlow::Importing { done: index, total };
            }
            Some("summary") => {
                self.import_current = None;
                // A summary with errors is a FAILED import, however normally
                // the stream ended — never present a partial migration as
                // complete (the engine keeps collecting per-item failures
                // precisely so this can be surfaced).
                match import_summary_outcome(item) {
                    Ok((imported, skipped)) => {
                        self.sync_flow = SyncFlow::ImportDone { imported, skipped };
                    }
                    Err(message) => {
                        self.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                        self.runtime_change_error = Some(message.into());
                    }
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn quit_for_runtime_change(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        if engine.mode() == EngineMode::InProcess {
            cx.quit();
            return;
        }
        if self.runtime_change_task.is_some() {
            return;
        }

        self.runtime_change_error = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let shutdown = Tokio::spawn(cx, async move {
            engine
                .client()
                .call(methods::STOP_ENGINE, serde_json::json!({}))
                .await
                .map_err(|err| err.to_string())?;
            wait_for_remote_engine_shutdown(ipc_socket, &data_dir, RUNTIME_CHANGE_TIMEOUT).await
        });
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match shutdown.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(_) => cx.quit(),
                    Err(err) => {
                        shell.runtime_change_error = Some(format!(
                            "Could not stop the remote engine: {err}. Run `cypher daemon stop`, then quit and reopen Cypher."
                        ).into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn start_sign_in(&mut self, cx: &mut Context<Self>) {
        let scope = self.state.read(cx).workspace_scope;
        if scope == Some(WorkspaceScope::Development) {
            return;
        }
        self.close_user_menu(cx);
        if scope == Some(WorkspaceScope::Local) {
            self.sync_flow = SyncFlow::Enabling;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.auth_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SIGN_IN, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| match result {
                Ok(value) => {
                    if let Some(url) = value.get("url").and_then(|u| u.as_str()) {
                        cx.open_url(url);
                    }
                    cx.notify();
                }
                Err(err) => {
                    if scope == Some(WorkspaceScope::Local) && shell.sync_flow == SyncFlow::Enabling
                    {
                        shell.sync_flow = SyncFlow::Idle;
                    }
                    shell.sidebar_notice = Some(format!("Sign in failed: {err}").into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- org gate ----

    fn ensure_org_ui(&mut self, cx: &mut Context<Self>) {
        if self.org.is_some() {
            return;
        }
        self.org = Some(OrgGateUi {
            orgs: Loadable::Idle,
            submitting: true,
            error: None,
            task: None,
        });
        self.load_orgs(cx);
    }

    fn load_orgs(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(org) = self.org.as_mut() else { return };
        org.orgs = Loadable::Loading;
        org.submitting = true;
        org.error = None;
        org.task = Some(cx.spawn(async move |this, cx| {
            let result: Result<Option<Vec<OrgRow>>, String> = async {
                let value = engine
                    .client()
                    .call(methods::LIST_ORGS, serde_json::json!({}))
                    .await
                    .map_err(|err| err.to_string())?;
                match org_setup(parse_orgs(&value)) {
                    OrgSetup::AutoCreate => {
                        engine
                            .client()
                            .call(
                                methods::CREATE_ORG,
                                serde_json::json!({ "name": DEFAULT_PERSONAL_ORG_NAME }),
                            )
                            .await
                            .map_err(|err| err.to_string())?;
                        Ok(None)
                    }
                    OrgSetup::AutoSelect(organization_id) => {
                        engine
                            .client()
                            .call(
                                methods::SELECT_ORG,
                                serde_json::json!({
                                    "organizationId": organization_id
                                }),
                            )
                            .await
                            .map_err(|err| err.to_string())?;
                        Ok(None)
                    }
                    OrgSetup::Pick(rows) => Ok(Some(rows)),
                }
            }
            .await;
            this.update(cx, |shell, cx| {
                if let Some(org) = shell.org.as_mut() {
                    match result {
                        Ok(Some(rows)) => {
                            org.orgs = Loadable::Ready(rows);
                            org.submitting = false;
                        }
                        Ok(None) => {
                            // CREATE_ORG and SELECT_ORG both re-scope auth. Keep
                            // the progress state visible until AuthStatus flips
                            // to SignedIn and removes the gate.
                            org.orgs = Loadable::Loading;
                        }
                        Err(err) => {
                            org.orgs = Loadable::Error(err);
                            org.submitting = false;
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select_org(&mut self, organization_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(org) = self.org.as_mut() else { return };
        org.submitting = true;
        org.error = None;
        org.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SELECT_ORG,
                    serde_json::json!({ "organizationId": organization_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if let Some(org) = shell.org.as_mut() {
                    org.submitting = false;
                    if let Err(err) = result {
                        org.error = Some(format!("{err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- render pieces ----

    /// Evaluate a width tween at "now" (manual drive — see [`WidthTween`]).
    /// Mid-flight: eased 200ms lerp, and `motion_active` is flagged so render
    /// schedules the next animation frame. Finished, stale, absent, or under
    /// reduced motion: exactly `target`.
    fn eval_tween(&self, tween: Option<WidthTween>, target: f32) -> f32 {
        let Some(WidthTween { from, to, started }) = tween else {
            return target;
        };
        if self.reduced_motion {
            return target;
        }
        let total = RESIZE.total();
        let raw = started.elapsed().as_secs_f32() / total.as_secs_f32();
        if raw >= 1.0 {
            return target;
        }
        self.motion_active.set(true);
        motion::lerp(from, to, RESIZE.progress(raw))
    }

    /// Animated width container: tweens 200ms ease-out on collapse/expand, and
    /// clips a fixed-width inner so content never reflows mid-transition.
    fn pane_container(
        &self,
        tween: Option<WidthTween>,
        target: f32,
        inner: AnyElement,
    ) -> AnyElement {
        div()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .w(px(self.eval_tween(tween, target)))
            .child(inner)
            .into_any_element()
    }

    /// The animated spacer clearing the macOS traffic lights ahead of a
    /// titlebar control cluster. Fullscreen toggles tween the cluster start
    /// over 200ms ease-out ([`RESIZE`]; reduced motion snaps).
    /// `None` off macOS — no phantom flex child.
    fn titlebar_spacer(&self, container_pad: f32) -> Option<AnyElement> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let fullscreen = self.fullscreen.unwrap_or(false);
        // The tween runs in cluster-start coordinates; the spacer is that
        // minus the container's own padding.
        let start = self.eval_tween(self.titlebar_tween, titlebar_cluster_start(fullscreen));
        let width = (start - container_pad).max(0.0);
        Some(div().flex_none().h_full().w(px(width)).into_any_element())
    }

    /// The header's content row with the animated left inset — the native port
    /// of zeron __root.tsx `transition-[padding-left] duration-200 ease-out` +
    /// `style={{ paddingLeft: headerInset }}`: on sidebar toggles (and macOS
    /// fullscreen flips) the SAME element's padding tweens, so the title
    /// glides to its new x-position. Route changes SNAP: the tween is killed
    /// by every route transition (zeron remounts the keyed header variants —
    /// instant swap, zero horizontal motion).
    /// Where unified-titlebar content (tabs / the settings label) starts: past
    /// the traffic lights + control cluster, riding the fullscreen inset tween.
    pub(super) fn title_bar_content_start(&self) -> f32 {
        let fullscreen = self.fullscreen.unwrap_or(false);
        let is_macos = cfg!(target_os = "macos");
        let cluster = self.eval_tween(
            self.titlebar_tween,
            cluster_buttons_start(is_macos, fullscreen),
        );
        cluster + CLUSTER_BUTTONS_WIDTH + 10.0
    }

    /// The unified window titlebar. Chat: only a drag strip over the
    /// SIDEBAR column's top band — the workspace tiles' own headers are the
    /// drag regions over the rest (a full-width strip would cover their
    /// tabs). Settings: the full-width band. The traffic lights and control
    /// cluster overlay its left end either way.
    fn render_title_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.route {
            Route::Chat => {
                let sidebar_now = self.eval_tween(self.sidebar_tween, self.sidebar_target());
                let bar = div()
                    .h(px(Theme::TITLEBAR_HEIGHT))
                    .w(px(sidebar_now))
                    .flex_none();
                self.titlebar_drag_region("chat-titlebar", bar, cx)
                    .into_any_element()
            }
            Route::Settings(_) => {
                let inner = div()
                    .size_full()
                    .flex()
                    .items_center()
                    .pt(px(Theme::TITLEBAR_TOP_PAD))
                    .pl(px(self.title_bar_content_start()))
                    .pr(px(titlebar_right_padding(
                        cfg!(target_os = "windows"),
                        Theme::SPACE_LG,
                    )));
                let bar = div().h(px(Theme::TITLEBAR_HEIGHT)).flex_none().child(inner);
                self.titlebar_drag_region("settings-header-titlebar", bar, cx)
                    .into_any_element()
            }
        }
    }

    /// Make a titlebar strip drag the window — zed's platform-titlebar
    /// pattern (zeron's `.drag` region): mark it a [`WindowControlArea::Drag`]
    /// (macOS app-owned titlebar), hand the drag to the compositor once the
    /// pointer moves with the button down, and double-click zooms.
    fn titlebar_drag_region(
        &self,
        id: impl Into<gpui::ElementId>,
        el: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.id(id)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.titlebar_should_move = false))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_should_move = false),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_should_move = true),
            )
            // Hand the drag to the compositor only while the button is
            // actually held (`pressed_button` guard): on macOS
            // `start_window_move` runs AppKit's NATIVE drag session
            // (`performWindowDragWithEvent:`), and AppKit resolves a quick
            // second click inside that session as a titlebar double-click —
            // system zoom — natively, beyond gpui's reach. Without the guard a
            // stale `titlebar_should_move` (armed by a down whose bubble was
            // later stopped) would start that session from a mere hover move
            // between the two clicks of a double-click.
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, _| {
                    if this.titlebar_should_move && event.pressed_button == Some(MouseButton::Left)
                    {
                        this.titlebar_should_move = false;
                        window.start_window_move();
                    }
                }),
            )
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    if cfg!(target_os = "macos") {
                        // Native titlebar double-click action (zoom/minimize
                        // per system preference).
                        window.titlebar_double_click();
                    } else {
                        window.zoom_window();
                    }
                }
            })
    }

    /// The ONE top-left window-control cluster (sidebar toggle + back/forward —
    /// zeron window-controls.tsx): rendered once, in a paint-only overlay layer
    /// pinned at the window's top-left, ABOVE the sidebar and headers. The
    /// sidebar width animates *beneath* it, so the buttons keep their element
    /// identity and never move or remount on collapse/expand; only the
    /// fullscreen traffic-light inset tweens (the animated spacer). The
    /// container has no id/listeners — everything between the buttons falls
    /// through to the titlebar drag strips below.
    fn render_titlebar_cluster(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let can_back = self.nav.can_back();
        let can_forward = self.nav.can_forward();
        // The new-session + joins the cluster while the sidebar is collapsed
        // (fading on the sidebar width tween) — INSIDE the cluster row so it
        // shares the buttons' exact size and 2px rhythm; a separate mount in
        // the title row sat 10px off the cluster and read misaligned (user
        // report).
        let plus_alpha = self.titlebar_plus_alpha();
        let show_plus = matches!(self.route, Route::Chat) && plus_alpha > 0.01;
        div()
            .absolute()
            .top_0()
            .left_0()
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .pt(px(Theme::TITLEBAR_TOP_PAD))
            .gap(px(2.0))
            .px(px(10.0))
            .children(self.titlebar_spacer(12.0))
            .child(window_control_button(
                "toggle-sidebar",
                icons::SIDEBAR_MINIMALISTIC_LEFT,
                &theme,
                cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)),
            ))
            .child(nav_history_button(
                "nav-back",
                icons::ARROW_LEFT,
                can_back,
                &theme,
                cx.listener(|this, _, _, cx| this.navigate_back(cx)),
            ))
            .child(nav_history_button(
                "nav-forward",
                icons::ARROW_RIGHT,
                can_forward,
                &theme,
                cx.listener(|this, _, _, cx| this.navigate_forward(cx)),
            ))
            .children(show_plus.then(|| {
                div()
                    .flex_none()
                    .opacity(plus_alpha)
                    .child(window_control_button(
                        "titlebar-new-session",
                        icons::PLUS,
                        &theme,
                        cx.listener(|this, _, _, cx| this.open_new_session(cx)),
                    ))
            }))
            // Workspace layout presets (Chat route only).
            .when(matches!(self.route, Route::Chat), |el| {
                el.child(self.render_layout_button(&theme, cx))
            })
            .into_any_element()
    }

    /// How present the titlebar's new-session + is: 0 with the sidebar open
    /// (the + lives in the sidebar header), 1 fully collapsed, riding the
    /// sidebar width tween in between.
    pub(super) fn titlebar_plus_alpha(&self) -> f32 {
        let sidebar_now = self.eval_tween(self.sidebar_tween, self.sidebar_target());
        let open_width = self.settings.sidebar_width.max(1.0);
        (1.0 - sidebar_now / open_width).clamp(0.0, 1.0)
    }

    /// Native Windows caption controls integrated into Cypher's unified
    /// titlebar. `WindowControlArea` maps these hit targets to HTMINBUTTON,
    /// HTMAXBUTTON, and HTCLOSE, so Windows owns their behavior (including
    /// Snap Layouts) while GPUI renders the system Segoe caption glyphs.
    fn render_windows_caption_controls(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        if !cfg!(target_os = "windows") {
            return None;
        }

        let theme = Theme::of(cx);
        let (maximize_id, maximize_glyph) = if window.is_maximized() {
            ("window-restore", "\u{e923}")
        } else {
            ("window-maximize", "\u{e922}")
        };
        Some(
            div()
                .id("windows-window-controls")
                .absolute()
                .top_0()
                .right_0()
                .h(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_row()
                .font_family("Segoe Fluent Icons")
                .child(windows_caption_button(
                    "window-minimize",
                    "\u{e921}",
                    WindowControlArea::Min,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    maximize_id,
                    maximize_glyph,
                    WindowControlArea::Max,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    "window-close",
                    "\u{e8bb}",
                    WindowControlArea::Close,
                    theme,
                    true,
                ))
                .into_any_element(),
        )
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = crate::surface_style::theme(crate::surface_style::Region::Sidebar, cx);
        let inner: AnyElement = match self.route {
            Route::Settings(section) => self.render_settings_nav(section, &theme, cx),
            Route::Chat => self.render_chat_sidebar(&theme, cx),
        };
        let target = self.sidebar_target();
        // Transparent — the sidebar sits directly on the frost shell; the main
        // card's gutter, tone, and shadow provide the separation without a
        // vertical divider. The content row spans the full window height (the
        // titlebar overlays it), so the column pads itself below the chrome.
        self.pane_container(
            self.sidebar_tween,
            target,
            div()
                .h_full()
                .pt(px(Theme::TITLEBAR_HEIGHT))
                .child(inner)
                .into_any_element(),
        )
    }

    /// Settings navigation has a pinned device selector, scrollable scope
    /// groups, and a pinned Back row. Only Device settings follow the selector;
    /// client preferences and workspace management keep their own scope.
    fn render_settings_nav(
        &mut self,
        section: SettingsSection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section_icon = |item: SettingsSection| match item {
            SettingsSection::Devices => icons::MONITOR,
            SettingsSection::Harnesses => icons::WIDGET,
            SettingsSection::Providers => icons::KEY_MINIMALISTIC,
            SettingsSection::Titles => icons::TUNING,
            SettingsSection::Agents => icons::KEY_MINIMALISTIC,
            SettingsSection::Commands => icons::COMMAND,
            SettingsSection::Mcp => icons::GLOBAL,
            SettingsSection::Subagents => icons::CHECKLIST,
            SettingsSection::Github => icons::GITHUB_MARK,
            SettingsSection::Appearance => icons::TUNING,
            SettingsSection::Notifications => icons::BELL,
            SettingsSection::Shortcuts => icons::KEYBOARD,
            SettingsSection::Archived => icons::ARCHIVE_MINIMALISTIC,
        };
        let mut groups = div()
            .id("settings-nav-groups")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(8.0))
            .pb(px(16.0));
        for (index, (title, sections)) in SettingsSection::NAV_GROUPS.into_iter().enumerate() {
            let rows = sections
                .iter()
                .copied()
                .map(|item| {
                    let selected = item == section;
                    div()
                        .id(SharedString::from(format!("settings-nav-{}", item.label())))
                        .role(gpui::Role::Button)
                        .aria_label(item.label())
                        .tab_index(0)
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .min_h(px(32.0))
                        .rounded(px(8.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .text_size(px(13.0))
                        .when(selected, |el| {
                            el.bg(crate::surface_style::sidebar_selected(theme))
                                .font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .text_color(if selected {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .cursor_pointer()
                        .hover(|s| {
                            s.bg(if selected {
                                crate::surface_style::sidebar_selected(theme)
                            } else {
                                crate::surface_style::sidebar_hover(theme)
                            })
                            .text_color(theme.text)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.open_settings(item, cx)))
                        .child(
                            icon(section_icon(item))
                                .size(px(16.0))
                                .text_color(if selected {
                                    theme.text
                                } else {
                                    theme.text_muted
                                }),
                        )
                        .child(SharedString::from(item.label()))
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            groups = groups.child(
                div()
                    .id(("settings-nav-group", index))
                    .flex()
                    .flex_col()
                    .when(index > 0, |el| {
                        el.mt(px(12.0))
                            .pt(px(12.0))
                            .border_t_1()
                            .border_color(theme.border)
                    })
                    .child(
                        div()
                            .px(px(8.0))
                            .pt(px(4.0))
                            .pb(px(8.0))
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child(title),
                    )
                    .child(div().flex().flex_col().gap(px(2.0)).children(rows)),
            );
        }
        // Follow the user-resized sidebar instead of imposing a header width.
        div()
            .w(px(self.settings.sidebar_width))
            .h_full()
            .min_h_0()
            .key_context("SettingsNavigation")
            .tab_group()
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(12.0))
                    .pt(px(12.0))
                    .pb(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .px(px(4.0))
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from("Settings")),
                    )
                    .child(self.settings_target.clone()),
            )
            .child(groups)
            // Neither the device selector nor Back scroll with the sections.
            .child(
                div()
                    .flex_none()
                    .px(px(8.0))
                    .py(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .id("settings-back")
                            .role(gpui::Role::Button)
                            .aria_label("Back to chats")
                            .tab_index(0)
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .rounded(px(8.0))
                            .px(px(Theme::SPACE_SM))
                            .py(px(6.0))
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .cursor_pointer()
                            .hover(|s| {
                                s.bg(crate::surface_style::sidebar_hover(theme))
                                    .text_color(theme.text)
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx)))
                            .child(
                                // AltArrowLeft chevron (zeron settings-sidebar.tsx),
                                // not the straight history arrow.
                                icon(icons::ALT_ARROW_LEFT)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Back")),
                    ),
            )
            .into_any_element()
    }

    /// One compact session row: agent mark + title on the left, status corner
    /// on the right (mini spinner while working, amber question mark while the
    /// run waits on an answer, emerald check for unseen finished turns,
    /// relative time otherwise). The row is inset from the project-card
    /// edge — or, when `nested` under a checkout section, from that
    /// section's rail; click selects and right-click opens the context
    /// menu. The branch is NOT repeated per row — it lives in the
    /// branch/worktree group header above (see [`spaces`](crate::shell::spaces)).
    /// `harness` is `None` when the sidebar hides agent marks (one runtime
    /// throughout): the title then starts in line with the project title.
    /// `Some(None)` keeps the mark's slot empty so titles stay aligned.
    #[allow(clippy::too_many_arguments)]
    fn render_chat_row(
        &self,
        id: String,
        title: SharedString,
        time_ago: SharedString,
        harness: Option<Option<cypher_proto::HarnessId>>,
        status: ChatIndicator,
        selected: bool,
        pinned: bool,
        nested: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Status corner shares the relative-time slot so the compact row's
        // width stays stable: spinner while working, amber question mark while
        // the agent waits on the user, emerald check for an unseen finished
        // turn ("ready for you"), time otherwise. The pulse clock drives the
        // spinner while it stays mounted.
        let corner: AnyElement = match status {
            ChatIndicator::Working => div()
                .flex_none()
                .child(loaders::mini_gradient_spinner(
                    format!("chat-working-{id}"),
                    2.0,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            // The turn is parked on a question, so the spinner has stopped —
            // without a corner of its own the row fell back to the relative
            // time and read exactly like an idle session (user report). Amber
            // is the tone the theme reserves for awaiting-input; the glyph is
            // slightly larger than the check because it carries inner detail.
            ChatIndicator::AwaitingInput => icon(icons::QUESTION_CIRCLE)
                .size(px(12.0))
                .flex_none()
                .text_color(theme.warning)
                .into_any_element(),
            ChatIndicator::Completed => icon(icons::CHECK)
                .size(px(11.0))
                .flex_none()
                .text_color(theme.success.opacity(0.9))
                .into_any_element(),
            _ => div()
                .flex_none()
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(time_ago)
                .into_any_element(),
        };
        let (hover, text) = (crate::surface_style::sidebar_hover(theme), theme.text);
        let selected_wash = crate::surface_style::sidebar_selected(theme);
        let subline = theme.text_muted.opacity(0.5);
        let select_id = id.clone();
        let menu_id = id.clone();
        let drag = workspace_view::TabDrag::new(
            crate::workspace::TabKey::session(id.clone()),
            title.clone(),
            harness.flatten(),
        );
        // Hover fades over transition-colors (zeron session-row.tsx) — both
        // the wash and the title brighten ride the same 150ms blend.
        let fade_key = format!("chat-row-{id}");
        let rest_bg = if selected {
            selected_wash
        } else {
            crate::theme::wash(0.0)
        };
        // A selected row must NOT drift toward the hover wash: in dark the two
        // fills are identical so the blend is a no-op, but light's hover sits
        // below its near-opaque selected fill, and blending toward it visibly
        // dimmed the active row under the pointer (user report).
        let hover_bg = if selected { selected_wash } else { hover };
        let rest_text = if selected { text } else { text.opacity(0.8) };
        div()
            .id(SharedString::from(format!("chat-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(8.0))
            .mx(px(6.0))
            .mb(px(2.0))
            // Sessions are children of the project header: keep the selected
            // wash inset 6px, then indent the content so the agent mark lands
            // beneath the project title rather than at the card's left edge.
            // Under a checkout section the rail already carries the indent.
            // Without an agent mark the title itself takes the mark's place,
            // landing on the project title's 33px column either way.
            .pl(px(match (nested, harness.is_some()) {
                (false, true) => 20.0,
                (false, false) => 27.0,
                (true, true) => 8.0,
                (true, false) => 11.0,
            }))
            .pr(px(8.0))
            .py(px(6.0))
            .text_color(motion::hover_blend(&fade_key, rest_text, text))
            .bg(motion::hover_blend(&fade_key, rest_bg, hover_bg))
            // No selection ring (user request) — the wash alone marks the
            // active row.
            .on_hover(motion::hover_listener(fade_key.clone()))
            .cursor_pointer()
            // ⌘-click (Ctrl elsewhere) opens it in a new split to the right.
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                let split = event.modifiers().secondary();
                this.open_chat_with(select_id.clone(), split, cx);
            }))
            // Drag into the workspace: onto a tab bar or tile centre (join)
            // or a tile edge (split). A plain click still opens it.
            .on_drag(drag, workspace_view::TabDrag::ghost)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.chat_menu.open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            )
            // Agent identity + title. The project card already owns host and
            // project identity, so sessions only repeat what distinguishes
            // one run from another.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .when_some(harness, |el, harness| {
                        el.child(match harness.map(crate::pickers::harness_brand_icon) {
                            Some((path, tint)) => icon(path)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(tint.unwrap_or(subline).opacity(0.82))
                                .into_any_element(),
                            None => div().size(px(14.0)).flex_none().into_any_element(),
                        })
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .line_height(px(16.0))
                            .child(title),
                    )
                    .when(pinned, |el| {
                        el.child(
                            icon(icons::PIN)
                                .size(px(11.0))
                                .flex_none()
                                .text_color(subline),
                        )
                    }),
            )
            // Status corner: spinner / unread check / relative time.
            .child(div().text_color(subline).child(corner))
            .into_any_element()
    }

    /// Chat-mode sidebar: Cypher / Add project header, project-grouped session
    /// cards (every host together), the notice strip, and the UserMenu (§1.6).
    fn render_chat_sidebar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (user, workspace_scope) = {
            let state = self.state.read(cx);
            (state.auth_user().cloned(), state.workspace_scope)
        };

        // Keyed rows: (stable key, estimated height, element) — the key + height
        // list drives the §1.6 resort FLIP diff below (attention-bucket
        // promotions glide; cleared rows just go).
        let keyed: Vec<(String, f32, AnyElement)> = self.render_active_rows(theme, cx);

        // Resort glide (§1.6 View Transitions parity): when the ORDER of a live
        // list changes (new activity resort, grouping flip), surviving rows
        // glide from their old y to the new one — layout is already at the new
        // position; the offset is a paint-only relative inset animated to 0
        // over 260ms cubic-bezier(0.22,1,0.36,1). New rows fade in; removals
        // just go (matching the original). First fill and chat switches (which
        // don't reorder) never animate.
        let order: Vec<(String, f32)> = keyed.iter().map(|(k, h, _)| (k.clone(), *h)).collect();
        if self.sidebar_prev_order != order {
            if !self.sidebar_prev_order.is_empty() {
                let offsets = resort_offsets(&self.sidebar_prev_order, &order, GROUP_CARD_GAP);
                let prev_keys: std::collections::HashSet<&str> = self
                    .sidebar_prev_order
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect();
                let new_keys: std::collections::HashSet<String> = order
                    .iter()
                    .filter(|(k, _)| !prev_keys.contains(k.as_str()))
                    .map(|(k, _)| k.clone())
                    .collect();
                if !offsets.is_empty() || !new_keys.is_empty() {
                    self.resort_epoch += 1;
                    self.sidebar_resort = offsets;
                    self.sidebar_new_keys = new_keys;
                }
            }
            self.sidebar_prev_order = order;
        }
        let epoch = self.resort_epoch;
        let list_items: Vec<AnyElement> = keyed
            .into_iter()
            .map(|(key, _, element)| {
                if let Some(dy) = self.sidebar_resort.get(&key).copied() {
                    let id = SharedString::from(format!("resort-{epoch}-{key}"));
                    div()
                        .child(element)
                        .with_animation(id, RESORT.animation(), move |el, t| {
                            el.relative().top(px(dy * (1.0 - t)))
                        })
                        .into_any_element()
                } else if self.sidebar_new_keys.contains(&key) {
                    let id = SharedString::from(format!("row-in-{epoch}-{key}"));
                    motion::fade_quick(id, div().child(element)).into_any_element()
                } else {
                    element
                }
            })
            .collect();

        let (user_line, trigger_subline, menu_identity): (
            SharedString,
            Option<SharedString>,
            SharedString,
        ) = match workspace_scope {
            Some(WorkspaceScope::Local) => {
                let line = if matches!(self.sync_flow, SyncFlow::RestartPending { .. }) {
                    "Sync ready after restart"
                } else {
                    "Local only"
                };
                (line.into(), None, "Stored on this device".into())
            }
            Some(WorkspaceScope::Development) => (
                "Development".into(),
                Some("Local development runtime".into()),
                "Authentication disabled".into(),
            ),
            Some(WorkspaceScope::Synced) | None => {
                let line: SharedString = user
                    .as_ref()
                    .map(|u| u.name.clone().unwrap_or_else(|| u.email.clone()).into())
                    .unwrap_or_else(|| SharedString::from("Not signed in"));
                let email = user
                    .as_ref()
                    .map(|u| SharedString::from(u.email.clone()))
                    .unwrap_or_else(|| line.clone());
                (line, None, email)
            }
        };
        let avatar_url = user
            .as_ref()
            .and_then(|u| u.avatar_url.clone())
            .map(SharedString::from);
        let user_menu = self.render_user_menu(
            user_line.clone(),
            trigger_subline,
            menu_identity,
            avatar_url,
            theme,
            cx,
        );

        // The fixed product header + Add project action lives ABOVE the scroll
        // region (it must stay reachable no matter how long the card list gets).
        let actions = self.render_sidebar_header(theme, cx);

        div()
            .w(px(self.settings.sidebar_width))
            .h_full()
            .flex()
            .flex_col()
            // (No titlebar strip: the unified window titlebar spans the whole
            // window above this column.)
            .child(actions)
            // The project-grouped card list scrolls inside an EdgeFade scope —
            // a true per-glyph gradient at active overflow edges. Glass-safe
            // (no painted overlay can fade content over see-through blur) and
            // equivalent on opaque themes: alpha→0 reveals the surface tone
            // underneath, same as the gradient overlays it replaced. Overflow
            // is read at PAINT time via the scroll handle — render-time gating
            // rode the previous frame's offset, so the last frame of a content
            // shrink (row archived while scrolled) left a phantom fade stuck
            // over an unscrollable list (user report).
            .child(
                crate::edge_fade::edge_faded(
                    SIDEBAR_GLASS_FADE_BAND,
                    true,
                    true,
                    div().relative().flex_1().min_h_0().child(
                        div()
                            .id("sidebar-lists")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar_scroll)
                            .px(px(Theme::SPACE_SM))
                            .flex()
                            .flex_col()
                            // No "Sessions" header (user request) — the list
                            // is the whole column; a little air stands in.
                            .pt(px(4.0))
                            .child(if !list_items.is_empty() {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(GROUP_CARD_GAP))
                                    .pb(px(Theme::SPACE_SM))
                                    .children(list_items)
                                    .into_any_element()
                            } else {
                                div()
                                    .px(px(Theme::SPACE_SM))
                                    .pb(px(Theme::SPACE_SM))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from("No projects yet"))
                                    .into_any_element()
                            }),
                    ),
                )
                .fade_overflow_y(&self.sidebar_scroll),
            )
            // Update strip (above the user menu; below the lists). App-wide
            // chrome — the main window's alone.
            .when_some(
                self.render_update_strip(theme, cx)
                    .filter(|_| !self.is_project_window()),
                |el, strip| el.child(strip),
            )
            .when_some(
                self.render_pi_update_strip(theme, cx)
                    .filter(|_| !self.is_project_window()),
                |el, strip| el.child(strip),
            )
            // Inline mutation-failure notice.
            .when_some(self.sidebar_notice.clone(), |el, notice| {
                el.child(
                    div()
                        .id("sidebar-notice")
                        .mx(px(Theme::SPACE_SM))
                        .mb(px(Theme::SPACE_SM))
                        .px(px(Theme::SPACE_SM))
                        .py(px(4.0))
                        .rounded(px(Theme::CONTROL_RADIUS))
                        .border_1()
                        .border_color(theme.danger)
                        .text_size(px(11.0))
                        .text_color(theme.danger)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar_notice = None;
                            cx.notify();
                        }))
                        .child(notice),
                )
            })
            .when(!self.is_project_window(), |el| {
                el.child(div().p(px(Theme::SPACE_SM)).flex_none().child(user_menu))
            })
            .into_any_element()
    }

    /// Update strip: shown above the user menu whenever the engine's
    /// UpdateStatus stream reports a newer release. On a macOS bundle install
    /// it drives the whole flow — click to download, then click to restart into
    /// the staged bundle. Elsewhere (managed/source installs) it is advisory
    /// (`cypher update`); click dismisses it for that version.
    fn render_update_strip(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mac_app = matches!(self.install, cypher_update::InstallKind::MacApp { .. });
        let view = update_strip_view(
            self.state.read(cx).update.as_ref(),
            cypher_update::current_version(),
            self.update_dismissed.as_deref(),
            mac_app,
            &self.update_flow,
        )?;
        let UpdateStripView {
            label,
            clickable,
            failed,
        } = view;
        let tone = if failed { theme.danger } else { theme.accent };
        // Dark-purple GLASS tint (user request), not the 400-level accent as
        // a fill: deep pigment at partial alpha tints the blur showing
        // through instead of compositing into the slab that a bright indigo
        // fill produced (earlier user report). Light chrome gets a lavender
        // accent wash instead — dark purple under indigo-600 text goes muddy.
        let (chip_bg, chip_bg_hover) = if failed {
            (theme.danger.opacity(0.14), theme.danger.opacity(0.22))
        } else {
            match theme.appearance {
                crate::theme::Appearance::Dark => {
                    let purple = crate::theme::oklch(0.35, 0.12, 277.0);
                    (purple.opacity(0.45), purple.opacity(0.60))
                }
                crate::theme::Appearance::Light => {
                    (theme.accent.opacity(0.10), theme.accent.opacity(0.16))
                }
            }
        };

        let mut strip = div()
            .id("update-strip")
            .mx(px(Theme::SPACE_SM))
            // No bottom margin: the user-menu block below carries its own
            // SPACE_SM padding — doubling it read as a hole (user report).
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(chip_bg)
            .flex()
            .flex_row()
            .items_center()
            .text_size(px(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(tone)
            .child(div().flex_1().min_w_0().child(label));
        if clickable {
            strip = strip
                .cursor_pointer()
                .hover(move |s| s.bg(chip_bg_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.on_update_strip_click(cx)));
        }
        Some(strip.into_any_element())
    }

    /// Pi CLI + extension update notification. The engine checks npm shortly
    /// after boot and every six hours; one click delegates the actual update
    /// to `pi update --all`, then the engine hot-reloads the Pi runtime.
    fn render_pi_update_strip(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let view =
            pi_update_strip_view(self.state.read(cx).pi_update.as_ref(), self.pi_update_busy)?;
        let PiUpdateStripView {
            label,
            clickable,
            failed,
        } = view;
        let tone = if failed { theme.danger } else { theme.accent };
        let (chip_bg, chip_bg_hover) = if failed {
            (theme.danger.opacity(0.14), theme.danger.opacity(0.22))
        } else {
            match theme.appearance {
                crate::theme::Appearance::Dark => {
                    let purple = crate::theme::oklch(0.35, 0.12, 277.0);
                    (purple.opacity(0.45), purple.opacity(0.60))
                }
                crate::theme::Appearance::Light => {
                    (theme.accent.opacity(0.10), theme.accent.opacity(0.16))
                }
            }
        };
        let mut strip = div()
            .id("pi-update-strip")
            .mx(px(Theme::SPACE_SM))
            .mt(px(6.0))
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(chip_bg)
            .flex()
            .flex_row()
            .items_center()
            .text_size(px(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(tone)
            .child(div().flex_1().min_w_0().child(label));
        if clickable {
            strip = strip
                .cursor_pointer()
                .hover(move |s| s.bg(chip_bg_hover))
                .on_click(cx.listener(|this, _, _, cx| this.begin_pi_update(cx)));
        }
        Some(strip.into_any_element())
    }

    fn begin_pi_update(&mut self, cx: &mut Context<Self>) {
        if self.pi_update_busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.pi_update_busy = true;
        let state = self.state.clone();
        self.pi_update_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::APPLY_PI_UPDATES, serde_json::json!({}))
                .await;
            if let Ok(value) = &result {
                match serde_json::from_value::<cypher_engine::pi_packages::PiUpdateStatus>(
                    value.clone(),
                ) {
                    Ok(status) => {
                        state.update(cx, |state, cx| {
                            state.apply_pi_update(status);
                            cx.notify();
                        });
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "malformed ApplyPiUpdates reply");
                    }
                }
            }
            this.update(cx, |shell, cx| {
                shell.pi_update_busy = false;
                if let Err(err) = result {
                    shell.sidebar_notice = Some(format!("Pi update failed: {err}").into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Idle → download; Ready → swap + relaunch; Failed → retry; advisory
    /// installs → dismiss for this version.
    fn on_update_strip_click(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.install, cypher_update::InstallKind::MacApp { .. }) {
            self.update_dismissed = self
                .state
                .read(cx)
                .update
                .as_ref()
                .and_then(|s| s.latest_version.clone());
            cx.notify();
            return;
        }
        match std::mem::replace(&mut self.update_flow, UpdateFlow::Idle) {
            UpdateFlow::Idle | UpdateFlow::Failed(_) => self.begin_update_download(cx),
            UpdateFlow::Downloading => self.update_flow = UpdateFlow::Downloading,
            UpdateFlow::Ready(staged) => self.apply_staged_update(staged, cx),
        }
    }

    /// Fetch the manifest and stage the new Cypher desktop bundle under the data dir
    /// (tokio — reqwest); the strip flips to "restart to apply" when done.
    fn begin_update_download(&mut self, cx: &mut Context<Self>) {
        let edge_url = self.boot.edge_url.clone();
        let data_dir = self.data_dir.clone();
        self.update_flow = UpdateFlow::Downloading;
        let download = Tokio::spawn(cx, async move {
            let manifest = cypher_update::fetch_latest(&edge_url).await?;
            cypher_update::stage_mac_app(&edge_url, &manifest, &data_dir).await
        });
        self.update_task = Some(cx.spawn(async move |this, cx| {
            let outcome = match download.await {
                Ok(Ok(staged)) => Ok(staged),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join_err) => Err(join_err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.update_flow = match outcome {
                    Ok(staged) => UpdateFlow::Ready(staged),
                    Err(message) => {
                        tracing::warn!(%message, "update download failed");
                        UpdateFlow::Failed(message.into())
                    }
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Swap the staged bundle over the installed one, arm the detached
    /// relauncher, and quit — the relauncher `open`s the new bundle once this
    /// process (and its engine lock / IPC socket) is gone.
    fn apply_staged_update(&mut self, staged: PathBuf, cx: &mut Context<Self>) {
        let cypher_update::InstallKind::MacApp { bundle } = self.install.clone() else {
            return;
        };
        match cypher_update::apply_mac_app(&staged, &bundle) {
            Ok(()) => {
                cypher_update::relaunch_app_after_exit(&bundle);
                cx.quit();
            }
            Err(err) => {
                tracing::error!(error = %err, "update apply failed");
                self.update_flow = UpdateFlow::Failed(format!("{err:#}").into());
                cx.notify();
            }
        }
    }

    fn open_about(&mut self, cx: &mut Context<Self>) {
        let check = about_check_from_status(
            self.state.read(cx).update.as_ref(),
            cypher_update::current_version(),
        );
        self.about = Some(AboutDialog {
            check,
            runtime_checking: false,
        });
        cx.notify();
    }

    fn begin_update_check(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.about.as_ref().map(|about| &about.check),
            Some(AboutCheck::Checking)
        ) {
            return;
        }
        let check = AboutCheck::Checking;
        if let Some(about) = &mut self.about {
            about.check = check;
            about.runtime_checking = true;
        } else {
            self.about = Some(AboutDialog {
                check,
                runtime_checking: true,
            });
        }
        self.begin_runtime_update_check(cx);
        let engine = self.state.read(cx).engine().cloned();
        let edge_url = self.boot.edge_url.clone();
        let state = self.state.clone();
        self.about_task = Some(cx.spawn(async move |this, cx| {
            let status = if let Some(engine) = engine {
                match engine
                    .client()
                    .call(methods::CHECK_UPDATE, serde_json::json!({}))
                    .await
                {
                    Ok(value) => serde_json::from_value::<cypher_update::UpdateStatus>(value).ok(),
                    Err(_) => None,
                }
            } else {
                None
            };
            let status =
                match status {
                    Some(status) => status,
                    None => match Tokio::spawn(cx, async move {
                        cypher_update::fetch_latest(&edge_url).await
                    })
                    .await
                    {
                        Ok(Ok(manifest)) => cypher_update::UpdateStatus {
                            current_version: cypher_update::current_version().into(),
                            update_available: cypher_update::version_newer(
                                &manifest.version,
                                cypher_update::current_version(),
                            ),
                            latest_version: Some(manifest.version),
                            checked_at: Some(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as i64)
                                    .unwrap_or(0),
                            ),
                            error: None,
                            relaunch_pending: false,
                        },
                        Ok(Err(err)) => cypher_update::UpdateStatus {
                            current_version: cypher_update::current_version().into(),
                            latest_version: None,
                            update_available: false,
                            checked_at: None,
                            error: Some(format!("{err:#}")),
                            relaunch_pending: false,
                        },
                        Err(err) => cypher_update::UpdateStatus {
                            current_version: cypher_update::current_version().into(),
                            latest_version: None,
                            update_available: false,
                            checked_at: None,
                            error: Some(err.to_string()),
                            relaunch_pending: false,
                        },
                    },
                };
            state.update(cx, |state, cx| {
                state.apply_update(status.clone());
                cx.notify();
            });
            this.update(cx, |shell, cx| {
                if let Some(about) = &mut shell.about {
                    about.check =
                        about_check_from_status(Some(&status), cypher_update::current_version());
                    if matches!(about.check, AboutCheck::Idle) {
                        about.check = AboutCheck::Current;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The Runtime half of a manual "Check for Updates": one on-demand sweep
    /// of the Runtime manifest, on its own task so a multi-minute bundle
    /// download never holds up the application check's answer.
    fn begin_runtime_update_check(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            if let Some(about) = &mut self.about {
                about.runtime_checking = false;
            }
            return;
        };
        let state = self.state.clone();
        self.about_runtime_task = Some(cx.spawn(async move |this, cx| {
            let reply = engine
                .client()
                .call(methods::CHECK_PI_UPDATE, serde_json::json!({}))
                .await;
            match reply {
                Ok(value) => {
                    match serde_json::from_value::<cypher_engine::pi_packages::PiUpdateStatus>(
                        value,
                    ) {
                        Ok(status) => {
                            state.update(cx, |state, cx| {
                                state.apply_pi_update(status);
                                cx.notify();
                            });
                        }
                        Err(err) => tracing::warn!(error = %err, "malformed CheckPiUpdate reply"),
                    }
                }
                // The check keeps running in the engine; the live
                // PiUpdateStatus watch stays the display authority.
                Err(err) => tracing::warn!(error = %err, "Pi Runtime update check failed"),
            }
            this.update(cx, |shell, cx| {
                if let Some(about) = &mut shell.about {
                    about.runtime_checking = false;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Scope-aware sidebar identity and account menu. Local runtimes advertise
    /// their storage boundary and offer sync; synced runtimes offer sign-out.
    fn render_user_menu(
        &mut self,
        user_line: SharedString,
        trigger_subline: Option<SharedString>,
        menu_identity: SharedString,
        avatar_url: Option<SharedString>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.user_menu.is_open();
        let action = account_menu_action(self.state.read(cx).workspace_scope, self.sync_flow);
        // Bottom-of-sidebar identity: avatar circle + scope/account label and
        // its secondary status line.
        let initial: SharedString = user_line
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into())
            .into();
        // Avatar: the GitHub/WorkOS profile picture when one is on the
        // account, else (and on any load failure) the white circle with the
        // initial in near-black (zeron user-menu.tsx).
        let fallback_avatar = {
            let initial = initial.clone();
            let theme = theme.clone();
            move || {
                div()
                    .size(px(26.0))
                    .flex_none()
                    .rounded_full()
                    .bg(theme.text)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.bg)
                    .child(initial.clone())
                    .into_any_element()
            }
        };
        // Final UI-side guard: only a bounded HTTPS URL may reach gpui's
        // `img` — anything else renders the initial-letter avatar instead and
        // can never be interpreted as a local file.
        let avatar = match safe_avatar_url(avatar_url) {
            Some(url) => {
                let loading = fallback_avatar.clone();
                gpui::img(url)
                    .size(px(26.0))
                    .flex_none()
                    .rounded_full()
                    .object_fit(gpui::ObjectFit::Cover)
                    .with_loading(loading)
                    .with_fallback(fallback_avatar)
                    .into_any_element()
            }
            None => fallback_avatar(),
        };
        let mut trigger = div()
            .id("user-menu")
            .flex_none()
            .rounded(px(8.0))
            .px(px(Theme::SPACE_SM))
            .py(px(Theme::SPACE_SM))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            // user-menu.tsx trigger: hover `bg-white/[0.04]`, open state
            // (`data-[state=open]`) the slightly stronger `bg-white/[0.06]`;
            // the hover wash fades over `transition-colors`.
            .bg(if open {
                theme.glass_hover()
            } else {
                motion::hover_blend(
                    "user-menu-trigger",
                    theme.glass_hover().opacity(0.0),
                    theme.glass_hover().opacity(0.8),
                )
            })
            .on_hover(motion::hover_listener("user-menu-trigger"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.user_menu.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                // A press that found the menu open closes it (the card's
                // mouse-down-out already began the close) — never reopen.
                if this.user_menu.take_press_was_open() {
                    this.close_user_menu(cx);
                } else {
                    this.user_menu.open(());
                }
                cx.notify();
            }))
            .child(avatar)
            .child(
                // Name with an optional status line underneath — no chip on the right.
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .line_height(px(17.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .truncate()
                            .child(user_line.clone()),
                    )
                    .when_some(trigger_subline, |identity, subline| {
                        identity.child(
                            div()
                                .text_size(px(11.0))
                                .line_height(px(15.0))
                                .text_color(theme.text_muted)
                                .child(subline),
                        )
                    }),
            );
        if self.user_menu.get().is_some() {
            // Floating menus stay on the overall palette, not the sidebar's
            // independently overridden foreground/background pair.
            let popup_theme = Theme::of(cx).clone();
            let theme = &popup_theme;
            let closing = self.user_menu.closing_since();
            // user-menu.tsx content: `w-[--radix-dropdown-menu-trigger-width]`
            // (exactly as wide as the trigger row — sidebar minus its p-2
            // gutters), `flex-col gap-0.5`, then: one small muted email line
            // (`px-2 pb-1 pt-1.5 text-[11px] text-muted-foreground/70`),
            // the action selected by the runtime scope, then "Settings".
            let menu = popover::popover_card(theme)
                .w(px(self.settings.sidebar_width - 2.0 * Theme::SPACE_SM))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_user_menu(cx);
                }))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .px(px(8.0))
                        .pt(px(6.0))
                        .pb(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.text_muted.opacity(0.7))
                        .truncate()
                        .child(menu_identity),
                )
                .when_some(action, |menu, action| {
                    let row = match action {
                        AccountMenuAction::EnableSync => {
                            popover::menu_row(theme, false, "user-menu-enable-sync")
                                .id("user-menu-enable-sync")
                                .on_click(cx.listener(|this, _, _, cx| this.start_sign_in(cx)))
                                .child(
                                    icon(icons::GLOBAL)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Enable sync"))
                                .into_any_element()
                        }
                        AccountMenuAction::SyncInProgress => {
                            popover::menu_row(theme, false, "user-menu-sync-progress")
                                .id("user-menu-sync-progress")
                                .opacity(0.6)
                                .child(
                                    icon(icons::GLOBAL)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Sync setup in progress"))
                                .into_any_element()
                        }
                        AccountMenuAction::RestartPending => {
                            popover::menu_row(theme, false, "user-menu-sync-restart")
                                .id("user-menu-sync-restart")
                                .on_click(cx.listener(|this, _, _, cx| this.reopen_sync_notice(cx)))
                                .child(
                                    icon(icons::RESTART)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Finish sync setup"))
                                .into_any_element()
                        }
                        AccountMenuAction::SignOut => {
                            popover::menu_row(theme, false, "user-menu-signout")
                                .id("user-menu-signout")
                                .on_click(cx.listener(|this, _, _, cx| this.request_sign_out(cx)))
                                .child(
                                    icon(icons::LOGOUT_2)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Sign out"))
                                .into_any_element()
                        }
                    };
                    menu.child(row).child(popover::menu_separator())
                })
                .child(
                    popover::menu_row(theme, false, "user-menu-settings")
                        .id("user-menu-settings")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_settings(SettingsSection::Harnesses, cx)
                        }))
                        .child(
                            icon(icons::SETTINGS_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from("Settings")),
                )
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_above(
                "user-menu-popover",
                menu,
                closing,
            ));
        }
        trigger.into_any_element()
    }

    fn render_sync_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let needs_org = matches!(
            self.state.read(cx).auth.as_ref(),
            Some(AuthState::NeedsOrganization { .. })
        );
        let remote_engine = self
            .state
            .read(cx)
            .engine()
            .is_some_and(|engine| matches!(engine.mode(), EngineMode::Remote { .. }));
        let runtime_change_label = if self.runtime_change_task.is_some() {
            "Stopping engine…"
        } else if remote_engine {
            "Stop daemon and quit"
        } else {
            "Quit Cypher"
        };

        if self.sync_flow == SyncFlow::Enabling && needs_org {
            return Some(self.render_org_gate(cx));
        }

        let signed_in_email: Option<SharedString> = match self.state.read(cx).auth.as_ref() {
            Some(AuthState::SignedIn { user, .. }) => Some(SharedString::from(user.email.clone())),
            _ => None,
        };
        // Spaces count as local work too: a projects-only profile must get
        // the import choice, not a bare "Switch now".
        let (local_chats, local_spaces) = {
            let state = self.state.read(cx);
            (state.chats.len(), state.spaces.len())
        };
        let work_phrase = local_work_phrase(local_chats, local_spaces);

        let card = match self.sync_flow {
            SyncFlow::Enabling => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Enable sync"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "Finish signing in in your browser. Cypher will keep using this local workspace until you quit and reopen.",
                    )),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "sync-enable-cancel")
                                .id("sync-enable-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_auth_setup(cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Open browser again")
                                .id("sync-enable-open-browser")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_sign_in(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::Canceling => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Canceling sync setup…"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "Removing the partial sign-in before returning to your local workspace.",
                    )),
                )
                .into_any_element(),
            // ── in-place switch wizard ────────────────────────────────────
            SyncFlow::SwitchOffer { notice_open: true } => {
                let has_local_work = work_phrase.is_some();
                let body: SharedString = match (&signed_in_email, &work_phrase) {
                    (Some(email), Some(phrase)) => format!(
                        "You're signed in as {email}. Bring {phrase} from this device into your synced workspace, or start it fresh."
                    )
                    .into(),
                    (Some(email), None) => format!(
                        "You're signed in as {email}. Cypher can switch to your synced workspace now."
                    )
                    .into(),
                    (None, Some(phrase)) => format!(
                        "Bring {phrase} from this device into your synced workspace, or start it fresh."
                    )
                    .into(),
                    (None, None) => "Cypher can switch to your synced workspace now.".into(),
                };
                let mut actions = div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Later", "sync-switch-later")
                            .id("sync-switch-later")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.postpone_sync_restart(cx)
                            })),
                    );
                if has_local_work {
                    actions = actions
                        .child(
                            popover::btn_ghost(&theme, "Start fresh", "sync-switch-fresh")
                                .id("sync-switch-fresh")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_synced_switch(false, cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Bring my work")
                                .id("sync-switch-import")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_synced_switch(true, cx)
                                })),
                        );
                } else {
                    actions = actions.child(
                        popover::btn_primary(&theme, "Switch now")
                            .id("sync-switch-now")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_synced_switch(false, cx)
                            })),
                    );
                }
                popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "Sync is ready"))
                    .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)))
                    .child(actions)
                    .into_any_element()
            }
            SyncFlow::Switching { import } => popover::dialog_card(&theme)
                .child(popover::dialog_title(
                    &theme,
                    "Switching to your synced workspace…",
                ))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    if import {
                        "Handing the engine over to your account. Your local sessions come along next."
                    } else {
                        "Handing the engine over to your account."
                    },
                )))
                .into_any_element(),
            SyncFlow::Importing { done, total } => {
                let fraction = if total == 0 {
                    0.0
                } else {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                };
                let label: SharedString = if total == 0 {
                    "Looking for local sessions…".into()
                } else {
                    format!("Importing session {} of {total}", (done + 1).min(total)).into()
                };
                let mut card = popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "Bringing your work over"))
                    .child(
                        div()
                            .mt(px(6.0))
                            .child(popover::dialog_body(&theme, label)),
                    );
                if let Some(current) = self.import_current.clone() {
                    card = card.child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(theme.text_muted)
                            .overflow_hidden()
                            .child(current),
                    );
                }
                card.child(
                    // Determinate progress: a hairline track with an accent fill.
                    div()
                        .mt(px(14.0))
                        .h(px(4.0))
                        .w_full()
                        .rounded(px(2.0))
                        .bg(theme.border)
                        .child(
                            div()
                                .h_full()
                                .rounded(px(2.0))
                                .bg(theme.accent_strong)
                                .w(gpui::relative(fraction.max(0.04))),
                        ),
                )
                .into_any_element()
            }
            SyncFlow::ImportDone { imported, skipped } => {
                let body: SharedString = match (imported, skipped) {
                    (0, 0) => "Your synced workspace is ready.".into(),
                    (n, 0) => format!(
                        "{n} session{} moved into your synced workspace.",
                        if n == 1 { "" } else { "s" },
                    )
                    .into(),
                    (n, s) => format!(
                        "{n} session{} imported, {s} already present.",
                        if n == 1 { "" } else { "s" },
                    )
                    .into(),
                };
                popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "You're all set"))
                    .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)))
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .child(
                                popover::btn_primary(&theme, "Continue")
                                    .id("sync-switch-done")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sync_flow = SyncFlow::Idle;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            SyncFlow::ImportFailed { notice_open: true } => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Import didn't finish"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    "Anything already imported is kept; retrying only copies what's missing.",
                )))
                .when_some(self.runtime_change_error.clone(), |card, error| {
                    card.child(
                        div()
                            .mt(px(10.0))
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Later", "import-failed-dismiss")
                                .id("import-failed-dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.postpone_sync_restart(cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Retry import")
                                .id("import-failed-retry")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.spawn_local_import(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::RestartPending { notice_open: true } => popover::dialog_card(&theme)
                .child(popover::dialog_title(
                    &theme,
                    "Sync needs a restart",
                ))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        if remote_engine {
                            "Cypher is using a background daemon. Stop it and quit Cypher, then reopen to start the synced workspace. Existing local sessions stay on this device and will not be uploaded."
                        } else {
                            "Quit and reopen Cypher to start the synced workspace. Existing local sessions stay on this device and will not be uploaded."
                        },
                    )),
                )
                .when_some(self.runtime_change_error.clone(), |card, error| {
                    card.child(
                        div()
                            .mt(px(10.0))
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Later", "sync-restart-later")
                                .id("sync-restart-later")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.postpone_sync_restart(cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, runtime_change_label)
                                .id("sync-restart-quit")
                                .when(self.runtime_change_task.is_some(), |button| {
                                    button.opacity(0.6)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.quit_for_runtime_change(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::SignOutConfirm => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Sign out?"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "Cypher will remove your credentials, close the synced workspace, and continue in local mode.",
                    )),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "signout-cancel")
                                .id("signout-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sync_flow = SyncFlow::Idle;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Sign out")
                                .id("signout-confirm")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_sign_out(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::SigningOut => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Signing out…"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "Removing account credentials and closing the synced workspace.",
                    )),
                )
                .into_any_element(),
            SyncFlow::Idle
            | SyncFlow::SwitchOffer { notice_open: false }
            | SyncFlow::ImportFailed { notice_open: false }
            | SyncFlow::RestartPending { notice_open: false }
            | SyncFlow::SignedOutRestartRequired => return None,
        };

        Some(popover::modal("sync-lifecycle-dialog", viewport, card))
    }

    /// Floating layers owned by the shell: context menus, edit dialogs, and
    /// the local-to-synced account lifecycle.
    fn render_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some((chat_id, position)) = self.chat_menu.get().cloned() {
            let chat_menu_closing = self.chat_menu.closing_since();
            let rename_id = chat_id.clone();
            let archive_id = chat_id.clone();
            let delete_id = chat_id.clone();
            let pin_id = chat_id.clone();
            let pinned = self
                .state
                .read(cx)
                .chats
                .iter()
                .any(|c| c.id == chat_id && c.pinned);
            let menu = popover::popover_card(&theme)
                .w(px(170.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_chat_menu(cx);
                }))
                .flex()
                .flex_col()
                .child(
                    popover::menu_row(&theme, false, format!("chat-menu-pin-{chat_id}"))
                        .id("chat-menu-pin")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_chat_pinned(pin_id.clone(), !pinned, cx)
                        }))
                        .child(icon(icons::PIN).size(px(16.0)).text_color(theme.text_muted))
                        .child(SharedString::from(if pinned { "Unpin" } else { "Pin" })),
                )
                .child(
                    popover::menu_row(&theme, false, format!("chat-menu-rename-{chat_id}"))
                        .id("chat-menu-rename")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_rename_chat(rename_id.clone(), cx)
                        }))
                        .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Rename…")),
                )
                .child(
                    popover::menu_row(&theme, false, format!("chat-menu-archive-{chat_id}"))
                        .id("chat-menu-archive")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.archive_chat(archive_id.clone(), cx)
                        }))
                        .child(
                            icon(icons::ARCHIVE_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from("Archive")),
                )
                .child(popover::menu_separator())
                .child(
                    popover::menu_row(&theme, false, format!("chat-menu-delete-{chat_id}"))
                        .id("chat-menu-delete")
                        .text_color(theme.danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_chat_menu(cx);
                            this.delete_confirm = Some(delete_id.clone());
                            cx.notify();
                        }))
                        .child(
                            icon(icons::TRASH_BIN_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.danger),
                        )
                        .child(SharedString::from("Delete…")),
                )
                .into_any_element();
            overlays.push(popover::menu_at(
                "chat-context-menu",
                position,
                menu,
                chat_menu_closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_dialog = None;
                        cx.notify();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename session"))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "rename-chat-cancel")
                                .id("rename-chat-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rename")
                                .id("rename-chat-save")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_chat(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-chat-dialog", viewport, card));
        }

        overlays.extend(self.render_space_overlays(viewport, window, cx));
        if let Some(overlay) = self.render_add_space_overlay(viewport, window, cx) {
            overlays.push(overlay);
        }

        if let Some(chat_id) = self.delete_confirm.clone() {
            let title = transcript::single_line(
                &self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|c| c.id == chat_id)
                    .and_then(|c| c.title.clone())
                    .unwrap_or_else(|| "New session".into()),
            );
            let card = popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Delete session?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    format!("\u{201C}{title}\u{201D} will be permanently deleted. This can\u{2019}t be undone."),
                )))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "delete-chat-cancel")
                                .id("delete-chat-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Delete")
                                .id("delete-chat-confirm")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_chat(chat_id.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("delete-chat-dialog", viewport, card));
        }

        if let Some(orphan) = self.delete_worktree_confirm.clone() {
            let label = orphan.label.clone();
            let card = popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Delete worktree too?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    format!(
                        "No other sessions are using the \u{201C}{label}\u{201D} worktree. Delete it as well? This can\u{2019}t be undone."
                    ),
                )))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Keep", "delete-worktree-keep")
                                .id("delete-worktree-keep")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_worktree_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Delete")
                                .id("delete-worktree-confirm")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_worktree(orphan.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("delete-worktree-dialog", viewport, card));
        }

        if let Some(sync) = self.render_sync_overlay(viewport, cx) {
            overlays.push(sync);
        }

        // The shared Comment pill/editor, LAST so it paints above every
        // clipped surface (transcript, diff panes, terminal).
        self.comment_popup.update(cx, |popup, cx| {
            if let Some(ui) = popup.render(window, cx) {
                overlays.push(ui);
            }
        });

        if self.showing_setup() {
            overlays.push(self.render_setup_overlay(cx));
        }

        if let Some(overlay) = self.render_about_overlay(viewport, cx) {
            overlays.push(overlay);
        }

        overlays
    }

    fn render_about_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let about = self.about.as_ref()?;
        let theme = Theme::of(cx).clone();
        let version = cypher_update::current_version();
        let install = install_kind_label(&self.install);
        let checking = matches!(about.check, AboutCheck::Checking);
        let runtime_line = about_runtime_line(
            self.state.read(cx).pi_update.as_ref(),
            about.runtime_checking,
        );
        let status = match &about.check {
            AboutCheck::Idle => None,
            AboutCheck::Checking => Some("Checking for updates…".to_string()),
            AboutCheck::Current => Some(format!("Cypher {version} is up to date.")),
            AboutCheck::Available { latest } => Some(format!("Update available — v{latest}")),
            AboutCheck::Failed { message } => Some(message.to_string()),
        };
        let failed = matches!(about.check, AboutCheck::Failed { .. });
        let mut card = popover::dialog_card(&theme)
            .id("about-cypher-dialog")
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.about = None;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .child(cypher_app_icon().size(px(72.0)).rounded(px(16.0)))
                    .child(popover::dialog_title(&theme, "Cypher"))
                    .child(popover::dialog_body(&theme, format!("Version {version}")))
                    .child(popover::dialog_body(&theme, install).text_color(theme.text_muted)),
            );
        if let Some(status) = status {
            card = card.child(div().mt(px(12.0)).w_full().child(
                popover::dialog_body(&theme, status).when(failed, |el| el.text_color(theme.danger)),
            ));
        }
        if let Some(line) = runtime_line {
            card = card.child(
                div()
                    .mt(px(4.0))
                    .w_full()
                    .child(popover::dialog_body(&theme, line).text_color(theme.text_muted)),
            );
        }
        card = card.child(
            div()
                .mt(px(16.0))
                .w_full()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(8.0))
                .child(
                    popover::btn_ghost(&theme, "OK", "about-cypher-ok")
                        .id("about-cypher-ok")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.about = None;
                            cx.notify();
                        })),
                )
                .child(
                    popover::btn_primary(
                        &theme,
                        if checking {
                            "Checking…"
                        } else {
                            "Check for Updates"
                        },
                    )
                    .id("about-cypher-check")
                    .when(!checking, |el| {
                        el.on_click(cx.listener(|this, _, _, cx| this.begin_update_check(cx)))
                    }),
                ),
        );
        Some(popover::modal(
            "about-cypher-dialog",
            viewport,
            card.into_any_element(),
        ))
    }

    /// Settings route: just the section outlet — the section label lives in
    /// the unified window titlebar (render_title_bar). Settings never
    /// underlaps: pad below the overlaid titlebar.
    fn render_settings_main(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let outlet = self.settings_outlet(section, cx);
        let card_bg = crate::chat_style::panel_background(
            crate::chat_style::settings(cx),
            Theme::of(cx),
            false,
        );
        // The same floating card the workspace tiles sit in. Stretched by
        // the row, not `h_full` — 100% plus the margins overflowed the
        // window by the bottom inset.
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .ml(px(4.0))
            .mt(px(PANEL_EDGE_INSET))
            .mb(px(PANEL_EDGE_INSET))
            .mr(px(PANEL_EDGE_INSET))
            .rounded(px(PANEL_CORNER_RADIUS))
            .bg(card_bg)
            .shadow_sm()
            .overflow_hidden()
            .pt(px(Theme::TITLEBAR_HEIGHT - PANEL_EDGE_INSET))
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(outlet))
            .into_any_element()
    }

    fn resize_handle<T>(
        &self,
        id: impl Into<gpui::ElementId>,
        marker: impl Fn() -> T,
        reset: fn(&mut Shell, &mut Context<Shell>),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div>
    where
        T: 'static,
    {
        div()
            .id(id)
            .w(px(5.0))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .on_drag(marker(), |_, _point: Point<gpui::Pixels>, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    if event.click_count == 2 {
                        reset(this, cx);
                        this.schedule_save(cx);
                        cx.notify();
                    }
                }),
            )
    }

    fn render_signed_out_restart(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let runtime_change_label = if self.runtime_change_task.is_some() {
            "Stopping engine…"
        } else {
            "Retry local mode"
        };
        let card = div()
            .w(px(380.0))
            .px(px(32.0))
            .py(px(40.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_card)
            .shadow_lg()
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            .child(
                cypher_app_icon()
                    .w(px(36.0))
                    .h(px(36.0)),
            )
            .child(
                div()
                    .mt(px(24.0))
                    .text_size(px(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from("Signed out")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .mb(px(24.0))
                    .text_size(px(13.0))
                    .line_height(px(19.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(
                        "Cypher removed your credentials but could not finish closing the previous synced workspace. Retry before continuing in local mode.",
                    )),
            )
            .when_some(self.runtime_change_error.clone(), |card, error| {
                card.child(
                    div()
                        .mb(px(16.0))
                        .text_size(px(12.0))
                        .line_height(px(17.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(
                popover::btn_primary(&theme, runtime_change_label)
                    .id("signed-out-quit")
                    .when(self.runtime_change_task.is_some(), |button| {
                        button.opacity(0.6)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_local_runtime_transition(false, cx)
                    })),
            );

        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(motion::fade_in("signed-out-restart", card)),
            )
            .into_any_element()
    }

    fn render_gate_card(&mut self, phase: &GatePhase, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let content: AnyElement = match phase {
            // Backend unreachable: quiet centered copy (zeron Gate `Failed`),
            // plus a Retry affordance (the native engine doesn't self-redial).
            GatePhase::Failed(error) => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(Theme::SPACE_MD))
                .child(
                    div()
                        .text_size(px(14.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(error.clone())),
                )
                .child(
                    div()
                        .id("retry-engine")
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(px(13.0))
                        .text_color(theme.text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.glass_hover()))
                        .on_click(cx.listener(|this, _, _, cx| this.retry_engine(cx)))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element(),
            // Login card (zeron App.tsx Gate): centered card on the grid —
            // logo, "Log in to Cypher", copy, full-width white Log in button.
            _ => div()
                .w(px(360.0))
                .px(px(32.0))
                .py(px(40.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_card)
                .shadow_lg()
                .flex()
                .flex_col()
                .items_center()
                .text_center()
                .child(
                    cypher_app_icon()
                        .w(px(36.0))
                        .h(px(36.0)),
                )
                .child(
                    div()
                        .mt(px(24.0))
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from("Log in to Cypher")),
                )
                .child(
                    div()
                        .mt(px(6.0))
                        .mb(px(24.0))
                        .text_size(px(13.0))
                        .line_height(px(19.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(
                            "This opens your browser to finish logging in — you'll come right back.",
                        )),
                )
                .child(
                    div()
                        .id("sign-in")
                        .w_full()
                        .h(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .bg(theme.text)
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.on_solid)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.9))
                        .on_click(cx.listener(|this, _, _, cx| this.start_sign_in(cx)))
                        .child(SharedString::from("Log in")),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .relative()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    // Keyed per phase (zeron App.tsx `<div key={phase}
                    // className="animate-in">`): every gate swap replays the
                    // 0.5s entrance instead of mutating one animated element.
                    .child(motion::fade_in(
                        match phase {
                            GatePhase::SignIn => "gate-card-signin",
                            _ => "gate-card-failed",
                        },
                        div().child(content),
                    )),
            )
            .into_any_element()
    }

    /// Organization onboarding used by the synced gate and, for a local
    /// runtime, only after the user explicitly starts the sync opt-in.
    fn render_org_gate(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_org_ui(cx);
        let theme = Theme::of(cx).clone();
        let local_setup = self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local);
        let Some(org) = self.org.as_ref() else {
            return Empty.into_any_element();
        };
        let submitting = org.submitting;
        let error = org.error.clone();
        let orgs = org.orgs.clone();

        let email: Option<SharedString> = self
            .state
            .read(cx)
            .auth_user()
            .map(|u| u.email.clone().into());

        let memberships: AnyElement =
            match &orgs {
                Loadable::Idle | Loadable::Loading => div()
                    .mt(px(24.0))
                    .child(popover::skeleton_rows(
                        "org-skeleton",
                        &theme,
                        2,
                        cx.entity_id(),
                        cx,
                    ))
                    .into_any_element(),
                Loadable::Error(message) => div()
                    .mt(px(24.0))
                    .child(
                        popover::error_row(&theme, message).child(
                            div()
                                .id("orgs-retry")
                                .px(px(Theme::SPACE_SM))
                                .py(px(3.0))
                                .rounded(px(Theme::CONTROL_RADIUS))
                                .border_1()
                                .border_color(theme.border)
                                .text_color(theme.text)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.glass_hover()))
                                .on_click(cx.listener(|this, _, _, cx| this.load_orgs(cx)))
                                .child(SharedString::from("Retry")),
                        ),
                    )
                    .into_any_element(),
                Loadable::Ready(rows) if rows.is_empty() => Empty.into_any_element(),
                Loadable::Ready(rows) => div()
                    .mt(px(24.0))
                    .flex()
                    .flex_col()
                    .child(div().flex().flex_col().gap(px(4.0)).children(
                        rows.iter().enumerate().map(|(ix, row)| {
                            let org_id = row.organization_id.clone();
                            div()
                                .id(("org-row", ix))
                                .px(px(12.0))
                                .py(px(8.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.bg)
                                .text_size(px(13.0))
                                .text_color(theme.text)
                                .when(submitting, |el| el.opacity(0.5))
                                .cursor_pointer()
                                .hover(|s| s.bg(crate::theme::wash(0.11)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_org(org_id.clone(), cx);
                                }))
                                .child(SharedString::from(row.name.clone()))
                        }),
                    ))
                    .into_any_element(),
            };

        let choosing = matches!(&orgs, Loadable::Ready(rows) if !rows.is_empty());
        let title = if choosing {
            "Choose a workspace"
        } else {
            "Setting up sync"
        };
        let blurb: SharedString = match (choosing, email) {
            (true, Some(email)) => {
                format!("Choose where to continue. Signed in as {email}.").into()
            }
            (true, None) => "Choose where to continue.".into(),
            (false, Some(email)) => format!("Preparing your synced workspace for {email}.").into(),
            (false, None) => "Preparing your synced workspace.".into(),
        };
        let card = div()
            .w(px(400.0))
            .px(px(32.0))
            .py(px(36.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_card)
            .shadow_lg()
            .flex()
            .flex_col()
            .child(cypher_app_icon().w(px(28.0)).h(px(28.0)))
            .child(
                div()
                    .mt(px(20.0))
                    .text_size(px(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(px(13.0))
                    .line_height(px(19.0))
                    .text_color(theme.text_muted)
                    .child(blurb),
            )
            .child(memberships)
            .when_some(error, |el, message| {
                el.child(
                    div()
                        .mt(px(16.0))
                        .text_size(px(12.0))
                        .line_height(px(17.0))
                        .text_color(theme.danger_muted.opacity(0.9)) // red-300
                        .child(message),
                )
            })
            .child(
                div().mt(px(24.0)).flex().flex_row().child(
                    div()
                        .id("org-signout")
                        .text_size(px(12.0))
                        .text_color(theme.text_muted.opacity(0.6))
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.text))
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_auth_setup(cx)))
                        .child(SharedString::from(if local_setup {
                            "Cancel sync setup"
                        } else {
                            "Use a different account"
                        })),
                ),
            );

        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(motion::fade_in("org-gate-card", card)),
            )
            .into_any_element()
    }
}

/// The sign-in gate's faint grid backdrop (zeron styles.css `.bg-grid`):
/// 44px hairlines at white 3.5%, with the radial mask approximated by edge
/// gradients back into the page background (gpui has no mask-image).
fn grid_backdrop(theme: &Theme) -> AnyElement {
    let line = crate::theme::hairline(0.035);
    let bg = theme.bg;
    const STEP: f32 = 44.0;
    const SPAN: f32 = 2640.0;
    let verticals = (1..(SPAN / STEP) as usize).map(|i| {
        div()
            .absolute()
            .left(px(i as f32 * STEP))
            .top_0()
            .bottom_0()
            .w(px(1.0))
            .bg(line)
    });
    let horizontals = (1..((SPAN * 0.75) / STEP) as usize).map(|i| {
        div()
            .absolute()
            .top(px(i as f32 * STEP))
            .left_0()
            .right_0()
            .h(px(1.0))
            .bg(line)
    });
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .children(verticals)
        .children(horizontals)
        // Mask approximation: fade the grid back into the background toward
        // the window edges (the original masks to an ellipse at 50% / 40%).
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(120.0))
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(260.0))
                .bg(gpui::linear_gradient(
                    0.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    270.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .into_any_element()
}

/// A size-6 icon button for the titlebar strip (zeron window-controls.tsx:
/// `grid size-6 place-items-center rounded-md text-muted-foreground`).
fn window_control_button(
    id: &'static str,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let muted = theme.text_muted;
    let fade_key = format!("window-control-{id}");
    div()
        .id(id)
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        // zeron window-controls.tsx: `transition-colors` — the wash fades.
        .bg(motion::hover_blend(
            &fade_key,
            theme.glass_hover().opacity(0.0),
            theme.glass_hover(),
        ))
        .on_hover(motion::hover_listener(fade_key))
        // Buttons in/over a titlebar drag strip must be EXCLUDED from the
        // strip's event surface entirely. `.occlude()` (gpui
        // `HitboxBehavior::BlockMouse`) makes the window hit-test STOP at the
        // button, so every `is_hovered`-guarded strip listener — the
        // mouse-down that arms the drag, the mouse-move that hands AppKit a
        // native drag session (`performWindowDragWithEvent:`, whose second
        // quick click zooms NATIVELY on macOS), and the `click_count == 2`
        // zoom handler — never fires with the pointer over a button. It also
        // removes the button's rect from the native Drag control-area
        // hit-test on Windows/Linux. The click-level stop_propagation is
        // zed's ButtonLike belt on top. Double-click on EMPTY strip space
        // still zooms — nothing occludes it there.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(icon_path).size(px(16.0)).text_color(muted))
}

const WINDOWS_CAPTION_BUTTON_WIDTH: f32 = 36.0;
const WINDOWS_CAPTION_WIDTH: f32 = WINDOWS_CAPTION_BUTTON_WIDTH * 3.0;

fn titlebar_right_padding(is_windows: bool, base: f32) -> f32 {
    base + if is_windows {
        WINDOWS_CAPTION_WIDTH
    } else {
        0.0
    }
}

/// A Windows-owned caption target using the same system glyphs and native
/// non-client hit-test areas as GPUI/Zed's platform titlebar.
fn windows_caption_button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    theme: &Theme,
    close: bool,
) -> impl IntoElement {
    let (hover_bg, hover_fg, active_bg, active_fg) = if close {
        let red: gpui::Hsla = gpui::rgb(0xe81123).into();
        (
            red,
            gpui::white(),
            red.opacity(0.8),
            gpui::white().opacity(0.8),
        )
    } else {
        (
            theme.glass_hover(),
            theme.text,
            theme.glass_hover().opacity(0.7),
            theme.text,
        )
    };
    div()
        .id(id)
        .w(px(WINDOWS_CAPTION_BUTTON_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.0))
        .text_color(theme.text)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(active_bg).text_color(active_fg))
        .occlude()
        .window_control_area(area)
        .child(glyph)
}

/// A titlebar history button (zeron window-controls.tsx): enabled it is a
/// normal window-control button; disabled it dims to 35% opacity and ignores
/// the pointer (`disabled:pointer-events-none disabled:opacity-35`).
fn nav_history_button(
    id: &'static str,
    icon_path: &'static str,
    enabled: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    if !enabled {
        return div()
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            // Even disabled it reads as a control — occlude so double-clicks
            // on it don't fall through to the titlebar strip's zoom handler.
            .occlude()
            .child(
                icon(icon_path)
                    .size(px(16.0))
                    .text_color(theme.text_muted.opacity(0.35)),
            )
            .into_any_element();
    }
    window_control_button(id, icon_path, theme, on_click).into_any_element()
}

/// A size-7 icon button for the main-panel header (zeron __root.tsx:
/// `grid size-7 place-items-center rounded-md text-muted-foreground`).
fn header_icon_button(
    id: impl Into<gpui::ElementId>,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.into();
    let muted = theme.text_muted;
    let fade_key = format!("header-icon-{id}");
    div()
        .id(id)
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        // zeron __root.tsx header buttons: `transition-colors`.
        .bg(motion::hover_blend(
            &fade_key,
            crate::theme::wash(0.0),
            crate::theme::wash(0.11),
        ))
        .on_hover(motion::hover_listener(fade_key))
        // Same occlusion + click-swallowing as [`window_control_button`]: this
        // button sits inside a tile header's titlebar drag region, so its
        // rect must be carved out of the strip's drag/double-click surface.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(icon_path).size(px(16.0)).text_color(muted))
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "dev-capture")]
        if !self.is_project_window() {
            crate::dev_capture::start_once(window.window_handle(), cx);
        }
        let foreground = window.is_window_active();
        let selected = matches!(self.route, Route::Chat)
            .then(|| self.state.read(cx).selected_chat.clone())
            .flatten();
        if let Some(activity) =
            self.notification_activity
                .sample(foreground, selected, std::time::Instant::now())
        {
            self.state.update(cx, |state, cx| {
                state.report_notification_activity(activity, cx)
            });
        }
        let weak = cx.entity().downgrade();
        let scroll_activity = crate::notification_activity::scroll_observer(move |cx| {
            let _ = weak.update(cx, |shell, cx| {
                if shell
                    .notification_activity
                    .interact(std::time::Instant::now())
                {
                    cx.notify();
                }
            });
        });
        // A sidebar chat selection can leave settings without close_settings.
        // Do not retain a hidden credential field in the cached page entity.
        if self.route != Route::Settings(SettingsSection::Providers)
            && let Some(page) = &self.providers_page
        {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        let theme = Theme::of(cx);
        // The shell tone (zeron `.frost`): the surface the sidebar sits on and
        // the main panel floats over as an inset rounded card. On macOS the
        // window background is the blurred desktop (lib.rs `Blurred`), so the
        // frost paints translucent — the sidebar and card margins read as
        // glass while the opaque card keeps text off it.
        let (frost, text, font) = (theme.glass(), theme.text, theme.font_sans.clone());
        let (workspace_scope, auth) = {
            let state = self.state.read(cx);
            (state.workspace_scope, state.auth.clone())
        };
        self.sync_flow = sync_flow_after_auth(self.sync_flow, workspace_scope, auth.as_ref());
        let restart_required = self.sync_flow == SyncFlow::SignedOutRestartRequired;
        let gate = self
            .debug_gate
            .clone()
            .unwrap_or_else(|| self.state.read(cx).gate());

        // Fullscreen hides the macOS traffic lights — reflow the control
        // cluster with a 200ms ease-out tween (§1.1). A fullscreen transition
        // resizes the window, which re-renders us, so polling here is exact.
        let fullscreen = window.is_fullscreen();
        if self.fullscreen != Some(fullscreen) {
            if self.fullscreen.is_some() && cfg!(target_os = "macos") {
                self.titlebar_tween = Some(WidthTween::new(
                    titlebar_cluster_start(!fullscreen),
                    titlebar_cluster_start(fullscreen),
                ));
            }
            self.fullscreen = Some(fullscreen);
        }
        // Manual tween drive bookkeeping for this pass (see [`WidthTween`]).
        self.reduced_motion = motion::reduced_motion(cx);
        self.motion_active.set(false);

        // Keyboard shortcuts (mod-s/b/j) dispatch through the window focus
        // chain — with nothing focused they go dead. Land initial focus on the
        // composer, and whenever focus is lost with no successor (e.g. the
        // focused element unmounted), route it back there.
        // The landing spot is the FOCUSED tile's composer (the root when
        // that tile is empty, so window shortcuts keep dispatching).
        if self.focus_sub.is_none() {
            self.focus_sub = Some(cx.on_focus_lost(window, |this: &mut Shell, window, cx| {
                match this.route {
                    Route::Chat if !this.showing_setup() => this.focus_landing(window, cx),
                    // No composer here — clear the stale handle so `focused()`
                    // reads None (the render hook below re-lands focus when the
                    // route returns to Chat; a lingering unmounted handle would
                    // otherwise dead-end keyboard dispatch for good).
                    Route::Chat | Route::Settings(_) => window.blur(),
                }
            }));
        }
        let chat_ready = !restart_required
            && matches!(gate, GatePhase::Ready)
            && matches!(self.route, Route::Chat)
            && !self.showing_setup();
        if chat_ready && (window.focused(cx).is_none() || std::mem::take(&mut self.focus_pending)) {
            self.focus_landing(window, cx);
        }
        // The find bar can close out from under the keyboard — selecting
        // another chat closes find inside the transcript, which unmounts the
        // field without ever firing a focus-lost event. Focus would then sit
        // on an element that no longer renders and every key would dead-end.
        let stranded: Vec<session::SlotId> = self
            .slots
            .iter()
            .filter(|(_, slot)| {
                !slot.transcript.read(cx).find_open()
                    && slot.find_input.focus_handle(cx).is_focused(window)
            })
            .map(|(sid, _)| *sid)
            .collect();
        for sid in stranded {
            if let Some(slot) = self.slots.get_mut(&sid) {
                slot.find_focus_pending = false;
            }
            match self.route {
                Route::Chat if !self.showing_setup() => self.focus_landing(window, cx),
                Route::Chat | Route::Settings(_) => window.blur(),
            }
        }

        let root = div()
            .id("shell-root")
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(frost)
            .text_color(text)
            .font_family(font)
            .text_size(px(14.0))
            .child(scroll_activity)
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                if this
                    .notification_activity
                    .interact(std::time::Instant::now())
                {
                    cx.notify();
                }
            }))
            .capture_key_down(cx.listener(|this, _, _, cx| {
                if this
                    .notification_activity
                    .interact(std::time::Instant::now())
                {
                    cx.notify();
                }
            }))
            .track_focus(&self.root_focus)
            .on_drag_move(cx.listener(Self::on_sidebar_drag))
            .on_drag_move(cx.listener(Self::on_dock_drag))
            .on_drag_move(cx.listener(Self::on_terminal_drag))
            .on_drag_move(cx.listener(Self::on_split_drag))
            // The panel shortcuts are chat-scoped chrome: in Settings they are
            // no-ops (zeron __root.tsx gates the hotkey on `!isSettings`, and
            // the terminal panel is only mounted on session routes). The
            // sidebar toggle stays live everywhere, as in the original.
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                if matches!(this.route, Route::Chat)
                    && let Some(sid) = this.focused_slot()
                {
                    this.toggle_terminal(sid, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            // New session works from anywhere — `open_new_session` routes back
            // to chat itself, so Settings is not a dead spot.
            .on_action(cx.listener(|this, _: &NewSession, _, cx| this.open_new_session(cx)))
            // Chat-scoped, unlike new-session — `cycle_session` holds the guard
            // and says why.
            .on_action(cx.listener(|this, _: &NextSession, _, cx| this.cycle_session(true, cx)))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| this.cycle_session(false, cx)))
            .on_action(cx.listener(|this, _: &ToggleChanges, _, cx| {
                if matches!(this.route, Route::Chat)
                    && let Some(sid) = this.focused_slot()
                {
                    this.toggle_dock(sid, cx)
                }
            }))
            // Workspace layout (customizable — see `apply_keymap`).
            .on_action(cx.listener(|this, _: &SplitRight, _, cx| {
                this.split_focused(crate::workspace::Edge::Right, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, _, cx| {
                this.split_focused(crate::workspace::Edge::Bottom, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Left, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusRight, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Right, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusUp, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Top, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusDown, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Bottom, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| this.close_focused_tab(cx)))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| this.toggle_zoom_focused(cx)))
            .on_action(cx.listener(|this, _: &FocusTile1, _, cx| this.focus_tile(0, cx)))
            .on_action(cx.listener(|this, _: &FocusTile2, _, cx| this.focus_tile(1, cx)))
            .on_action(cx.listener(|this, _: &FocusTile3, _, cx| this.focus_tile(2, cx)))
            .on_action(cx.listener(|this, _: &FocusTile4, _, cx| this.focus_tile(3, cx)))
            .on_action(cx.listener(|this, _: &FocusTile5, _, cx| this.focus_tile(4, cx)))
            .on_action(cx.listener(|this, _: &FocusTile6, _, cx| this.focus_tile(5, cx)))
            .on_action(cx.listener(|this, _: &FocusTile7, _, cx| this.focus_tile(6, cx)))
            .on_action(cx.listener(|this, _: &FocusTile8, _, cx| this.focus_tile(7, cx)))
            .on_action(cx.listener(|this, _: &FocusTile9, _, cx| this.focus_tile(8, cx)))
            .map(|root| {
                use crate::workspace::Preset;
                macro_rules! layout {
                    ($root:expr, $($action:ident => $preset:ident),* $(,)?) => {
                        $root$(.on_action(cx.listener(|this, _: &$action, _, cx| {
                            this.apply_layout_from_menu(Preset::$preset, cx)
                        })))*
                    };
                }
                layout!(
                    root,
                    LayoutSingle => Single,
                    LayoutColumns2 => Columns2,
                    LayoutRows2 => Rows2,
                    LayoutColumns3 => Columns3,
                    LayoutRows3 => Rows3,
                    LayoutGrid2x2 => Grid2x2,
                    LayoutGrid3x3 => Grid3x3,
                    LayoutTwoStackedPlusOne => TwoStackedPlusOne,
                    LayoutOnePlusTwoStacked => OnePlusTwoStacked,
                )
            })
            .on_action(
                cx.listener(|this, _: &crate::app_menus::OpenSettings, _, cx| {
                    this.open_settings(SettingsSection::Harnesses, cx);
                }),
            )
            // About / updates / adding projects are app-wide: a project
            // window hands them to the main window.
            .on_action(cx.listener(|this, _: &crate::app_menus::About, _, cx| {
                if this.is_project_window() {
                    this.forward_to_main(cx, |main, cx| main.open_about(cx));
                    return;
                }
                this.open_about(cx);
            }))
            .on_action(
                cx.listener(|this, _: &crate::app_menus::CheckForUpdates, _, cx| {
                    if this.is_project_window() {
                        this.forward_to_main(cx, |main, cx| main.begin_update_check(cx));
                        return;
                    }
                    this.begin_update_check(cx);
                }),
            )
            .on_action(cx.listener(|this, _: &AddSpacePalette, _, cx| {
                if this.is_project_window() {
                    this.forward_to_main(cx, |main, cx| {
                        if main.add_space.is_none() {
                            main.open_add_space(cx);
                        }
                    });
                } else if this.add_space.is_some() {
                    this.add_space = None;
                    cx.notify();
                } else {
                    this.open_add_space(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FindInChat, _, cx| {
                if let Some(sid) = this.focused_slot() {
                    this.open_find(sid, cx)
                }
            }))
            // Transcript/diff text takes no focus: a drag there leaves focus
            // on this root, outside every input's Copy binding. Edit → Copy
            // dispatches the action here; ⌘C arrives as a raw key (a global
            // binding would pre-empt the terminal's own raw ⌘C copy).
            .on_action(|_: &crate::composer::Copy, _, cx| {
                copy_surface_selection(cx);
            })
            .on_key_down(|event: &gpui::KeyDownEvent, _, cx| {
                let ks = &event.keystroke;
                let m = &ks.modifiers;
                if ks.key == "c"
                    && (m.platform || m.control)
                    && !(m.shift || m.alt || m.function)
                    && copy_surface_selection(cx)
                {
                    cx.stop_propagation();
                }
            });

        let render_gate = if restart_required {
            GatePhase::Loading
        } else {
            gate.clone()
        };
        let root = match &render_gate {
            GatePhase::Ready => {
                // Focus is a sync signal: on the rising edge of window
                // activation, nudge every open room to verify liveness — a
                // broadcast-deaf socket (accepted writes, runtime pongs,
                // nothing delivered; 2026-08-04 incident) then heals within
                // seconds of the user looking at the app rather than waiting
                // out the background probe cadence.
                let window_active = window.is_window_active();
                if window_active && !self.was_window_active {
                    self.state.update(cx, |s, cx| s.probe_sync(cx));
                    // Platforms release independently, so the build you want
                    // may have shipped while you were away. The engine rate
                    // limits this, so the rising edge is safe to forward every
                    // time; it wakes the checker and never blocks on the
                    // network.
                    if let Some(engine) = self.state.read(cx).engine().cloned() {
                        cx.background_spawn(async move {
                            let _ = engine
                                .client()
                                .call(methods::UPDATE_ON_ACTIVATION, serde_json::json!({}))
                                .await;
                        })
                        .detach();
                    }
                }
                self.was_window_active = window_active;
                // A run finishing while you're LOOKING at the session must not
                // badge "completed" until you leave and return — mark it seen
                // live while the window is active (idempotent guard inside;
                // one extra frame settles it).
                // Every VISIBLE tile's session counts.
                if window_active {
                    let unseen_visible: Vec<String> = {
                        let s = self.state.read(cx);
                        self.workspace
                            .visible_tabs()
                            .into_iter()
                            .filter_map(|tab| tab.chat_id())
                            .filter(|id| s.chats.iter().any(|c| c.id == *id && c.unseen()))
                            .map(str::to_string)
                            .collect()
                    };
                    for chat_id in unseen_visible {
                        self.state
                            .update(cx, |s, cx| s.mark_chat_seen(&chat_id, cx));
                    }
                }
                // Capture knob: `CYPHER_OPEN_DIALOG=model` pops the combined
                // harness/model menu (needs `window`, so it fires here rather
                // than in `on_state_changed`).
                if self.debug_dialog.as_deref() == Some("model")
                    && let Some(composer) = self
                        .focused_slot()
                        .and_then(|sid| self.slots.get(&sid))
                        .map(|slot| slot.composer.clone())
                {
                    self.debug_dialog = None;
                    composer.update(cx, |c, cx| c.debug_open_model_menu(window, cx));
                }
                let sidebar = self.render_sidebar(cx);
                let sidebar_handle = self.resize_handle(
                    "sidebar-resize",
                    || SidebarResize,
                    |shell, _| shell.settings.sidebar_width = SIDEBAR_DEFAULT,
                    cx,
                );
                // Chat: the workspace of session tiles; Settings: the section
                // outlet. The per-session state stays intact for the return
                // trip.
                let main = match self.route {
                    Route::Chat => self.render_workspace(window, cx),
                    Route::Settings(section) => self.render_settings_main(section, cx),
                };
                let overlays = self.render_overlays(window.viewport_size(), window, cx);
                // The whole app page is one keyed `animate-in` entrance (zeron
                // App.tsx `<div key={phase} className="animate-in h-full">`):
                // arriving from the splash or any gate fades the page in; the
                // splash-out crossfades over it on boot.
                // The sidebar resize handle FLOATS over the sidebar/workspace
                // seam (zero layout width) so the sidebar's right gutter stays
                // exactly as wide as its left one — a 5px flex child here read
                // as lopsided spacing.
                let sidebar_seam = div()
                    .w(px(0.0))
                    .h_full()
                    .flex_none()
                    .relative()
                    .child(sidebar_handle.absolute().top_0().bottom_0().left(px(-2.0)));
                let title_bar = self.render_title_bar(cx);
                // Two columns: sidebar | workspace (or settings). The content
                // row spans the FULL window height — the titlebar overlays it
                // (glass, no fill); the sidebar pads itself down, and the
                // top-row tiles' headers sit in the titlebar band.
                let page = div()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_row()
                            .child(sidebar)
                            .child(sidebar_seam)
                            .child(main),
                    )
                    .child(div().absolute().top_0().left_0().right_0().child(title_bar))
                    .child(self.render_titlebar_cluster(cx))
                    .children(overlays);
                root.child(motion::fade_in("phase-app", page))
            }
            GatePhase::Loading => root, // splash overlay covers boot
            GatePhase::OrgGate => {
                let card = self.render_org_gate(cx);
                root.child(card)
            }
            phase @ (GatePhase::Failed(_) | GatePhase::SignIn) => {
                let card = self.render_gate_card(phase, cx);
                root.child(card)
            }
        };
        let root = if restart_required {
            let restart = self.render_signed_out_restart(cx);
            root.child(restart)
        } else {
            root
        };

        // A manually-driven tween is mid-flight: keep frames coming (the same
        // scheduling `with_animation` would have requested). Hover color fades
        // ride the same clock; their once-per-frame tick lives here (this is
        // the window's root render — it runs exactly once per frame).
        if self.motion_active.get() | motion::hover_fades_active() {
            window.request_animation_frame();
        }

        // Boot splash overlay: visible → crossfades out on Ready → removed.
        let root = match self.splash {
            SplashPhase::Visible => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, false))
            }
            SplashPhase::FadingOut => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, true))
            }
            SplashPhase::Gone => root,
        };

        // Caption controls are shell-level chrome, not Ready-page content:
        // keep them above the splash and every auth/org/error gate as well as
        // the full application. Gate pages also need a native drag surface
        // because they do not render the unified tabs/settings titlebar.
        let root = if (!restart_required && matches!(gate, GatePhase::Ready))
            || !cfg!(target_os = "windows")
        {
            root
        } else {
            root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(Theme::TITLEBAR_HEIGHT))
                    .window_control_area(WindowControlArea::Drag),
            )
        };
        root.children(self.render_windows_caption_controls(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn unix_engine_directory_does_not_redirect_client_preferences(cx: &mut gpui::TestAppContext) {
        let ui = tempfile::tempdir().unwrap();
        let engine = tempfile::tempdir().unwrap();
        let ui_settings = UiSettings {
            sidebar_width: 333.,
            ..Default::default()
        };
        let engine_settings = UiSettings {
            sidebar_width: 222.,
            ..Default::default()
        };
        ui_settings.save(ui.path()).unwrap();
        engine_settings.save(engine.path()).unwrap();
        let engine_bytes = std::fs::read(UiSettings::path(engine.path())).unwrap();
        cx.update(|cx| {
            gpui_tokio::init(cx);
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
            crate::composer::init(cx);
            crate::terminal::panel::init(cx);
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| state.data_dir = Some(ui.path().into()));
            let boot = EngineBootConfig {
                data_dir: engine.path().into(),
                ipc_socket: cypher_env::ipc_socket(engine.path()).unwrap(),
                edge_url: "http://127.0.0.1:1".into(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: cypher_proto::HarnessId::Mock,
            };
            let shell = cx.new(|cx| Shell::new(state.clone(), boot, ui.path().into(), cx));
            assert_eq!(shell.read(cx).data_dir, ui.path());
            assert_eq!(shell.read(cx).boot.data_dir, engine.path());
            assert_eq!(shell.read(cx).settings.sidebar_width, 333.);
            assert_eq!(state.read(cx).data_dir.as_deref(), Some(ui.path()));
            shell
                .read(cx)
                .settings
                .save(&shell.read(cx).data_dir)
                .unwrap();
        });
        assert_eq!(
            std::fs::read(UiSettings::path(engine.path())).unwrap(),
            engine_bytes
        );
    }

    #[test]
    fn every_default_shortcut_binds_on_this_platform() {
        // `apply_keymap` silently falls back on an unparseable combo, so a
        // default gpui cannot parse would ship as a dead shortcut.
        for id in crate::settings::ShortcutId::ALL {
            let combo = platform_combo(id.default_combo());
            assert!(
                Keystroke::parse(&combo).is_ok(),
                "{} default {combo:?} does not parse",
                id.label()
            );
        }
    }

    fn test_chat(id: &str, archived: bool) -> cypher_proto::Chat {
        cypher_proto::Chat {
            id: id.into(),
            title: Some(format!("Chat {id}")),
            archived,
            created_at: Utc::now(),
            ..crate::test_fixtures::chat()
        }
    }

    fn test_shell(cx: &mut gpui::TestAppContext) -> (Entity<Shell>, Entity<AppState>) {
        test_shell_in(tempfile::tempdir().unwrap().keep(), cx)
    }

    fn test_shell_in(
        ui: PathBuf,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<Shell>, Entity<AppState>) {
        cx.update(|cx| {
            gpui_tokio::init(cx);
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
            crate::composer::init(cx);
            crate::terminal::panel::init(cx);
            let state = cx.new(|_| AppState::new());
            let boot = EngineBootConfig {
                data_dir: ui.clone(),
                ipc_socket: cypher_env::ipc_socket(&ui).unwrap(),
                edge_url: "http://127.0.0.1:1".into(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: cypher_proto::HarnessId::Mock,
            };
            let shell = cx.new(|cx| Shell::new(state.clone(), boot, ui.clone(), cx));
            (shell, state)
        })
    }

    fn set_chats(
        state: &Entity<AppState>,
        chats: Vec<cypher_proto::Chat>,
        cx: &mut gpui::TestAppContext,
    ) {
        state.update(cx, |s, cx| {
            s.apply_chats(chats);
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn tab_shape(shell: &Entity<Shell>, cx: &mut gpui::TestAppContext) -> Vec<Vec<String>> {
        shell.read_with(cx, |shell, _| {
            shell
                .workspace
                .groups_in_reading_order()
                .into_iter()
                .map(|id| {
                    shell
                        .workspace
                        .group(id)
                        .unwrap()
                        .tabs()
                        .iter()
                        .map(|tab| match tab {
                            crate::workspace::TabKey::Session(id) => id.clone(),
                            crate::workspace::TabKey::NewSession(_) => "+".into(),
                        })
                        .collect()
                })
                .collect()
        })
    }

    #[gpui::test]
    fn boot_lands_the_latest_chat_as_a_tab_and_main_follows_focus(cx: &mut gpui::TestAppContext) {
        let (shell, state) = test_shell(cx);
        assert!(!state.read_with(cx, |s, _| s.transcript_watches()));
        set_chats(
            &state,
            vec![test_chat("a", false), test_chat("b", false)],
            cx,
        );
        let landed = tab_shape(&shell, cx);
        assert_eq!(landed.len(), 1);
        assert_eq!(landed[0].len(), 1);
        let first = landed[0][0].clone();
        assert_eq!(
            state.read_with(cx, |s, _| s.selected_chat.clone()),
            Some(first.clone())
        );
        // Sidebar click: opens in the focused tile; ⌘-click splits right.
        let other = if first == "a" { "b" } else { "a" };
        shell.update(cx, |shell, cx| shell.open_chat(other.into(), cx));
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec![first.clone(), other.into()]]
        );
        assert_eq!(
            state
                .read_with(cx, |s, _| s.selected_chat.clone())
                .as_deref(),
            Some(other)
        );
        shell.update(cx, |shell, cx| {
            shell.open_chat_with(first.clone(), true, cx)
        });
        // Already open: focused where it is, never duplicated.
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec![first.clone(), other.into()]]
        );
        assert_eq!(
            state.read_with(cx, |s, _| s.selected_chat.clone()),
            Some(first.clone())
        );
        shell.update(cx, |shell, cx| shell.close_focused_tab(cx));
        assert_eq!(tab_shape(&shell, cx), vec![vec![other.to_string()]]);
        shell.update(cx, |shell, cx| {
            shell.open_chat_with(first.clone(), true, cx)
        });
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec![other.to_string()], vec![first.clone()]]
        );
        // One slot per tab, each pinned to its own session.
        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.slots.len(), 2);
            for slot in shell.slots.values() {
                assert_eq!(
                    slot.state.read(cx).selected_chat.as_deref(),
                    slot.tab.chat_id()
                );
            }
        });
    }

    #[gpui::test]
    fn a_canvas_becomes_its_session_and_vanished_or_archived_chats_close(
        cx: &mut gpui::TestAppContext,
    ) {
        let (shell, state) = test_shell(cx);
        set_chats(&state, vec![test_chat("a", false)], cx);
        shell.update(cx, |shell, cx| shell.open_new_session(cx));
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec!["a".to_string(), "+".into()]]
        );
        let canvas = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
        // The first send selects the minted chat on the canvas's context.
        let ctx = shell.read_with(cx, |shell, _| shell.slots[&canvas].state.clone());
        ctx.update(cx, |s, cx| s.select_chat(Some("new".into()), cx));
        cx.run_until_parked();
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec!["a".to_string(), "new".into()]]
        );
        // Same slot, re-keyed in place.
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.focused_slot(), Some(canvas));
        });
        assert_eq!(
            state
                .read_with(cx, |s, _| s.selected_chat.clone())
                .as_deref(),
            Some("new")
        );
        // A tile's context moving to ANOTHER chat (a subagent child) goes
        // back to its own; the other opens as a tab.
        set_chats(
            &state,
            vec![
                test_chat("a", false),
                test_chat("new", false),
                test_chat("c", false),
            ],
            cx,
        );
        ctx.update(cx, |s, cx| s.select_chat(Some("c".into()), cx));
        cx.run_until_parked();
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec!["a".to_string(), "new".into(), "c".into()]]
        );
        assert_eq!(
            ctx.read_with(cx, |s, _| s.selected_chat.clone()).as_deref(),
            Some("new")
        );
        // Deleted elsewhere / archived: the tabs close.
        set_chats(
            &state,
            vec![test_chat("a", false), test_chat("c", true)],
            cx,
        );
        assert_eq!(tab_shape(&shell, cx), vec![vec!["a".to_string()]]);
        shell.read_with(cx, |shell, _| assert_eq!(shell.slots.len(), 1));
    }

    #[gpui::test]
    fn background_tabs_of_deleted_chats_close_on_the_chats_frame(cx: &mut gpui::TestAppContext) {
        use crate::workspace::TabKey;
        let (shell, state) = test_shell(cx);
        // The boot landing may open either chat first: compare sorted.
        let tabs = |cx: &mut gpui::TestAppContext| {
            let mut shape = tab_shape(&shell, cx);
            shape.iter_mut().for_each(|group| group.sort());
            shape
        };
        set_chats(
            &state,
            vec![test_chat("a", false), test_chat("b", false)],
            cx,
        );
        shell.update(cx, |shell, cx| {
            shell.open_chat("a".into(), cx);
            shell.open_chat("b".into(), cx);
            shell.open_chat("a".into(), cx);
            // A fork this window just created, opened in the background.
            shell.expect_chat("fork");
            let group = shell.workspace.focused();
            shell.workspace.open_in(group, TabKey::session("fork"));
            shell.open_chat("a".into(), cx);
        });
        shell.read_with(cx, |shell, _| {
            assert!(shell.slot_for_tab(&TabKey::session("fork")).is_none());
        });
        // A notify that isn't a chats frame judges nothing missing.
        state.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(tabs(cx), vec![vec!["a", "b", "fork"]]);
        // "b" was deleted elsewhere: its background tab (with its slot)
        // closes; the fork's row just hasn't landed yet.
        set_chats(&state, vec![test_chat("a", false)], cx);
        assert_eq!(tabs(cx), vec![vec!["a", "fork"]]);
        // Listed once: no longer expected, so a later frame without it
        // closes it.
        set_chats(
            &state,
            vec![test_chat("a", false), test_chat("fork", false)],
            cx,
        );
        shell.read_with(cx, |shell, _| assert!(!shell.chat_expected("fork")));
        set_chats(&state, vec![test_chat("a", false)], cx);
        assert_eq!(tabs(cx), vec![vec!["a"]]);
    }

    #[gpui::test]
    fn boot_restores_the_saved_layout_and_docks(cx: &mut gpui::TestAppContext) {
        use crate::workspace::{Edge, TabKey, Workspace};
        let ui = tempfile::tempdir().unwrap().keep();
        let mut saved = Workspace::new();
        let left = saved.open(TabKey::session("a"));
        saved.open(TabKey::session("x")); // deleted since
        let canvas = saved.new_session_tab();
        saved.open(canvas);
        saved.open_split(TabKey::session("b"), left, Edge::Right);
        saved.open(TabKey::session("c"));
        saved.activate(saved.focused(), 0); // "b" active and focused
        UiSettings {
            workspace: Some(saved),
            session_docks: std::collections::HashMap::from([(
                "b".to_string(),
                crate::settings::SessionDock {
                    right: Some(0.5),
                    right_open: true,
                    used_at: 1,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        }
        .save(&ui)
        .unwrap();
        let (shell, state) = test_shell_in(ui.clone(), cx);
        set_chats(
            &state,
            vec![
                test_chat("a", false),
                test_chat("b", false),
                test_chat("c", false),
                test_chat("latest", false),
            ],
            cx,
        );
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec!["a".to_string()], vec!["b".into(), "c".into()]],
            "the saved layout wins over the boot landing"
        );
        assert_eq!(
            state
                .read_with(cx, |s, _| s.selected_chat.clone())
                .as_deref(),
            Some("b"),
            "main follows the restored focus"
        );
        shell.read_with(cx, |shell, _| {
            // Slots only for what's on screen; "c" gets one when shown.
            let mut open: Vec<_> = shell.slots.values().map(|slot| slot.tab.clone()).collect();
            open.sort_by_key(|tab| tab.chat_id().map(str::to_string));
            assert_eq!(open, vec![TabKey::session("a"), TabKey::session("b")]);
            let b = shell.focused_slot().unwrap();
            let slot = &shell.slots[&b];
            assert!(slot.dock.open, "b's dock was open");
            assert_eq!(slot.right_fraction, Some(0.5));
            // A session without an entry starts from the latest sizes, closed.
            let a = shell.slot_for_tab(&TabKey::session("a")).unwrap();
            assert!(!shell.slots[&a].dock.open);
            assert_eq!(shell.slots[&a].right_fraction, Some(0.5));
        });
        // Showing "c" creates its slot; the change persists.
        shell.update(cx, |shell, cx| shell.open_chat("c".into(), cx));
        let c = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
        shell.update(cx, |shell, cx| shell.toggle_dock(c, cx));
        cx.executor()
            .advance_clock(Duration::from_millis(SAVE_DEBOUNCE_MS + 50));
        cx.run_until_parked();
        let written = UiSettings::load(&ui);
        let ws = written.workspace.expect("layout saved");
        assert_eq!(ws.focused_tab(), Some(&TabKey::session("c")));
        assert_eq!(ws.tabs().count(), 3);
        assert!(written.session_docks["c"].right_open);
    }

    /// A ⌘N before the first chats frame joins the restored layout instead
    /// of replacing it, and the next save keeps the saved tabs.
    #[gpui::test]
    fn a_presync_canvas_joins_the_restored_layout(cx: &mut gpui::TestAppContext) {
        use crate::workspace::{Edge, TabKey, Workspace};
        let ui = tempfile::tempdir().unwrap().keep();
        let mut saved = Workspace::new();
        let left = saved.open(TabKey::session("a"));
        saved.open_split(TabKey::session("b"), left, Edge::Right);
        UiSettings {
            workspace: Some(saved),
            ..Default::default()
        }
        .save(&ui)
        .unwrap();
        let (shell, state) = test_shell_in(ui.clone(), cx);
        shell.update(cx, |shell, cx| shell.open_new_session(cx));
        let canvas = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
        set_chats(
            &state,
            vec![test_chat("a", false), test_chat("b", false)],
            cx,
        );
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec!["a".to_string()], vec!["b".into(), "+".into()]]
        );
        shell.read_with(cx, |shell, _| {
            // The same canvas slot, re-keyed into the restored layout.
            assert_eq!(shell.focused_slot(), Some(canvas));
            assert!(matches!(shell.slots[&canvas].tab, TabKey::NewSession(_)));
        });
        shell.update(cx, |shell, cx| shell.save_layout(cx));
        cx.executor()
            .advance_clock(Duration::from_millis(SAVE_DEBOUNCE_MS + 50));
        cx.run_until_parked();
        let ws = UiSettings::load(&ui).workspace.expect("layout saved");
        assert!(ws.contains(&TabKey::session("a")) && ws.contains(&TabKey::session("b")));
    }

    /// Zoomed: only the zoomed tile gets a slot; the hidden ones get theirs
    /// when shown.
    #[gpui::test]
    fn hidden_tiles_start_no_context(cx: &mut gpui::TestAppContext) {
        use crate::workspace::{Edge, TabKey, Workspace};
        let ui = tempfile::tempdir().unwrap().keep();
        let mut saved = Workspace::new();
        let left = saved.open(TabKey::session("a"));
        let right = saved
            .open_split(TabKey::session("b"), left, Edge::Right)
            .unwrap();
        saved.toggle_zoom(right);
        UiSettings {
            workspace: Some(saved),
            ..Default::default()
        }
        .save(&ui)
        .unwrap();
        let (shell, state) = test_shell_in(ui, cx);
        set_chats(
            &state,
            vec![test_chat("a", false), test_chat("b", false)],
            cx,
        );
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.slots.len(), 1);
            assert!(shell.slot_for_tab(&TabKey::session("b")).is_some());
        });
        shell.update(cx, |shell, cx| shell.toggle_zoom_focused(cx));
        shell.read_with(cx, |shell, _| assert_eq!(shell.slots.len(), 2));
    }

    /// Any workspace change that takes a session off screen (here a sidebar
    /// pick into its tile) closes the shared comment popup; one that hides
    /// nothing leaves it.
    #[gpui::test]
    fn a_session_leaving_the_screen_drops_the_comment_popup(cx: &mut gpui::TestAppContext) {
        let (shell, state) = test_shell(cx);
        set_chats(
            &state,
            vec![test_chat("a", false), test_chat("b", false)],
            cx,
        );
        shell.update(cx, |shell, cx| {
            shell.open_chat("a".into(), cx);
            shell.split_focused(crate::workspace::Edge::Right, cx);
        });
        let popup = shell.read_with(cx, |shell, _| shell.comment_popup.clone());
        let offer = |cx: &mut gpui::TestAppContext| {
            popup.update(cx, |popup, cx| {
                popup.offer(
                    "a".into(),
                    "quote".into(),
                    None,
                    gpui::point(px(10.0), px(10.0)),
                    crate::comments::CommentOwner::next_terminal(),
                    None,
                    std::rc::Rc::new(|_| {}),
                    None,
                    cx,
                )
            });
        };
        // Focusing the other (empty) tile hides nothing.
        offer(cx);
        let groups = shell.read_with(cx, |shell, _| shell.workspace.groups_in_reading_order());
        shell.update(cx, |shell, cx| shell.focus_group(groups[0], cx));
        assert!(popup.read_with(cx, |popup, _| popup.is_active()));
        // A sidebar pick replaces "a" in its tile: "a" leaves the screen.
        shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
        let mut first = tab_shape(&shell, cx)[0].clone();
        first.sort();
        assert_eq!(first, vec!["a".to_string(), "b".into()]);
        assert!(!popup.read_with(cx, |popup, _| popup.is_active()));
    }

    /// A slow (remote) canvas send outliving the pending-send overlay: while
    /// its composer is still sending, a chats frame without the minted row
    /// neither closes the tab nor leaves its context deselected.
    #[gpui::test]
    fn a_tab_stays_while_its_first_send_is_in_flight(cx: &mut gpui::TestAppContext) {
        let (shell, state) = test_shell(cx);
        set_chats(&state, vec![test_chat("a", false)], cx);
        shell.update(cx, |shell, cx| shell.open_new_session(cx));
        let sid = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
        let (ctx, composer) = shell.read_with(cx, |shell, _| {
            let slot = &shell.slots[&sid];
            (slot.state.clone(), slot.composer.clone())
        });
        composer.update(cx, |composer, _| composer.set_sending_for_test(true));
        ctx.update(cx, |s, cx| s.select_chat(Some("new".into()), cx));
        cx.run_until_parked();
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec!["a".to_string(), "new".into()]]
        );
        // No pending-send overlay (expired): only the composer holds it.
        set_chats(&state, vec![test_chat("a", false)], cx);
        assert_eq!(
            tab_shape(&shell, cx),
            vec![vec!["a".to_string(), "new".into()]]
        );
        assert_eq!(
            ctx.read_with(cx, |s, _| s.selected_chat.clone()).as_deref(),
            Some("new")
        );
        // The send settled and the row never came: the next frame closes it.
        composer.update(cx, |composer, _| composer.set_sending_for_test(false));
        set_chats(&state, vec![test_chat("a", false)], cx);
        assert_eq!(tab_shape(&shell, cx), vec![vec!["a".to_string()]]);
    }

    /// Render smoke test: the Ready page with a split workspace, docks and
    /// a terminal paints without panicking, and each tile measures its own
    /// session area.
    #[gpui::test]
    fn a_split_workspace_renders(cx: &mut gpui::TestAppContext) {
        let ui = tempfile::tempdir().unwrap().keep();
        cx.update(|cx| {
            gpui_tokio::init(cx);
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
            crate::composer::init(cx);
            crate::terminal::panel::init(cx);
        });
        let state = cx.new(|_| AppState::new());
        let boot = EngineBootConfig {
            data_dir: ui.clone(),
            ipc_socket: cypher_env::ipc_socket(&ui).unwrap(),
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: cypher_proto::HarnessId::Mock,
        };
        let (shell, vcx) = cx.add_window_view({
            let state = state.clone();
            move |_, cx| Shell::new(state, boot, ui, cx)
        });
        state.update(vcx, |s, cx| {
            s.connection = ConnectionStatus::Ready;
            s.workspace_scope = Some(WorkspaceScope::Local);
            s.auth = Some(AuthState::SignedOut);
            s.apply_chats(vec![test_chat("a", false), test_chat("b", false)]);
            cx.notify();
        });
        vcx.run_until_parked();
        shell.update(vcx, |shell, cx| {
            shell.setup_dismissed = true;
            // Boot landed one of them; the other splits off to the right.
            shell.open_chat_with("a".into(), true, cx);
            shell.open_chat_with("b".into(), true, cx);
            let sid = shell.focused_slot().unwrap();
            shell.toggle_dock(sid, cx);
            shell.split_focused(crate::workspace::Edge::Bottom, cx);
        });
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        let focused = shell.read_with(vcx, |shell, _| shell.focused_slot());
        assert_eq!(focused, None, "the new split is empty and focused");
        vcx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.open_new_session(cx);
                let sid = shell.focused_slot().unwrap();
                shell.toggle_terminal(sid, window, cx);
                shell
                    .workspace
                    .apply_preset(crate::workspace::Preset::Grid2x2);
                shell.workspace_changed(cx);
            });
            window.refresh();
        });
        vcx.run_until_parked();
        shell.read_with(vcx, |shell, _| {
            assert_eq!(shell.slots.len(), 3);
            let widths: Vec<f32> = shell
                .slots
                .values()
                .map(|slot| f32::from(slot.area.get().size.width))
                .collect();
            assert!(
                widths.iter().all(|w| *w > 0.0 && *w < 1000.0),
                "every tile measured its own area: {widths:?}"
            );
        });
    }

    #[test]
    fn workspace_shortcuts_parse() {
        let defaults = crate::settings::ShortcutId::ALL
            .into_iter()
            .map(|id| id.default_combo());
        for combo in defaults.chain(FOCUS_TILE_KEYS) {
            let keystroke = Keystroke::parse(&platform_combo(combo));
            assert!(keystroke.is_ok(), "{combo:?} does not parse");
        }
        assert_eq!(
            Keystroke::parse(&platform_combo("mod-\\")).unwrap().key,
            "\\"
        );
        // One tile action per key, in order.
        assert_eq!(focus_tile_action(0).name(), gpui::Action::name(&FocusTile1));
        assert_eq!(focus_tile_action(8).name(), gpui::Action::name(&FocusTile9));
    }

    #[test]
    fn safe_avatar_url_guards_https_and_bounds() {
        let ok = SharedString::from("https://avatars.example.com/a.png");
        assert_eq!(safe_avatar_url(Some(ok.clone())), Some(ok));
        // Non-HTTPS, hostless, malformed, and oversized values never reach
        // gpui's `img` (never a local-file interpretation either).
        for bad in [
            "http://avatars.example.com/a.png",
            "file:///etc/passwd",
            "not a url",
            "https://",
            "https:///a.png",
            // Malformed ports / hosts and embedded credentials: real URL
            // parsing rejects what a prefix check would accept.
            "https://host:99999/a.png",
            "https://host:abc/a.png",
            "https://user@host/a.png",
            "https://user:pass@host/a.png",
            &format!("https://host/{}", "a".repeat(2048)),
        ] {
            assert_eq!(
                safe_avatar_url(Some(SharedString::from(bad))),
                None,
                "{bad}"
            );
        }
        assert_eq!(safe_avatar_url(None), None);
        // Exactly 2048 chars is allowed.
        let at_cap = format!("https://h/{}", "a".repeat(2048 - "https://h/".len()));
        assert!(safe_avatar_url(Some(SharedString::from(at_cap))).is_some());
    }

    #[test]
    fn side_chat_start_error_text_is_actionable_for_unknown_method() {
        use cypher_rpc::RpcError;
        // Old engine on the hosting device: no `StartSideChat` method →
        // the message says what's wrong and what to do, not just an error.
        let unknown = Shell::side_chat_start_error_text(&RpcError::UnknownMethod(
            methods::START_SIDE_CHAT.to_string(),
        ));
        assert!(unknown.contains("newer Cypher engine"), "{unknown}");
        assert!(unknown.contains("device hosting this session"), "{unknown}");
        assert!(unknown.contains("Update that device"), "{unknown}");
        // A DIFFERENT unknown method is not the Side Chat feature gap.
        let other = Shell::side_chat_start_error_text(&RpcError::UnknownMethod("Nope".into()));
        assert_eq!(other, "Could not open Side Chat: unknown method: Nope");
        // Engine-side failures keep the generic prefix + raw error.
        assert_eq!(
            Shell::side_chat_start_error_text(&RpcError::Failed("engine exploded".into())),
            "Could not open Side Chat: engine exploded"
        );
        for generic in [
            RpcError::BadParams("bad".into()),
            RpcError::Transport("down".into()),
            RpcError::Closed,
        ] {
            assert!(
                Shell::side_chat_start_error_text(&generic)
                    .starts_with("Could not open Side Chat:"),
                "{generic}"
            );
        }
    }

    /// Session Fork v1: an old hosting engine answers UnknownMethod for
    /// `ForkSession` — the notice must say what's wrong and how to fix it.
    #[test]
    fn fork_session_error_text_is_actionable_for_unknown_method() {
        use cypher_rpc::RpcError;
        let unknown = Shell::fork_session_error_text(&RpcError::UnknownMethod(
            methods::FORK_SESSION.to_string(),
        ));
        assert!(unknown.contains("newer Cypher engine"), "{unknown}");
        assert!(unknown.contains("device hosting this session"), "{unknown}");
        assert!(unknown.contains("Update that device"), "{unknown}");
        // A DIFFERENT unknown method is not the Session Fork feature gap.
        assert_eq!(
            Shell::fork_session_error_text(&RpcError::UnknownMethod("Nope".into())),
            "Could not fork: unknown method: Nope"
        );
        // Engine-side failures keep the generic prefix + raw error.
        assert_eq!(
            Shell::fork_session_error_text(&RpcError::Failed("engine exploded".into())),
            "Could not fork: engine exploded"
        );
    }

    /// Session Fork idempotence: the `(sourceChatId, anchorMessageId) →
    /// requestId` mapping survives RPC errors / lost replies (so a retry
    /// reuses the SAME target id) and is dropped on a definitive reply
    /// (Created or typed Unavailable).
    #[test]
    fn fork_request_id_is_retained_after_errors_but_not_definitive_replies() {
        use cypher_rpc::RpcError;
        // Lost replies / transport errors: retain — the retry reuses the id.
        assert!(Shell::fork_request_id_retained(&Err(RpcError::Transport(
            "connection lost".into()
        ))));
        assert!(Shell::fork_request_id_retained(&Err(RpcError::Closed)));
        assert!(Shell::fork_request_id_retained(&Err(RpcError::Failed(
            "engine exploded".into()
        ))));
        assert!(Shell::fork_request_id_retained(&Err(
            RpcError::UnknownMethod(methods::FORK_SESSION.to_string())
        )));
        // A definitive reply (Created / typed Unavailable) drops the mapping.
        let chat = cypher_proto::Chat {
            id: "fork-x".into(),
            title: Some("Fork".into()),
            created_at: chrono::Utc::now(),
            room_gen: Some(2),
            ..crate::test_fixtures::chat()
        };
        assert!(!Shell::fork_request_id_retained(&Ok(
            cypher_proto::SessionForkResponse::Created(cypher_proto::SessionForkCreated {
                chat,
                mode: cypher_proto::SessionForkMode::EditUser,
                composer_text: None,
            })
        )));
        assert!(!Shell::fork_request_id_retained(&Ok(
            cypher_proto::SessionForkResponse::Unavailable(cypher_proto::SessionForkUnavailable {
                reason: cypher_proto::SessionForkUnavailableReason::LiveSession,
                message: "still running".into(),
            })
        )));
    }

    #[tokio::test]
    async fn remote_shutdown_waits_for_ipc_release() {
        let dir = tempfile::tempdir().unwrap();
        let port = cypher_env::ipc_socket(dir.path()).unwrap();
        let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(listener);
        });

        wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
            .await
            .unwrap();
        release.await.unwrap();
    }

    #[test]
    fn about_check_uses_app_version_and_last_check() {
        use cypher_update::UpdateStatus;
        let status = |latest: Option<&str>, checked: bool, error: Option<&str>| UpdateStatus {
            current_version: "0.1.0".into(),
            latest_version: latest.map(str::to_string),
            update_available: latest.is_some(),
            checked_at: checked.then_some(1),
            error: error.map(str::to_string),
            relaunch_pending: false,
        };
        assert_eq!(about_check_from_status(None, "0.1.0"), AboutCheck::Idle);
        assert_eq!(
            about_check_from_status(Some(&status(None, false, None)), "0.1.0"),
            AboutCheck::Idle
        );
        assert_eq!(
            about_check_from_status(Some(&status(Some("0.1.0"), true, None)), "0.1.0"),
            AboutCheck::Current
        );
        assert_eq!(
            about_check_from_status(Some(&status(Some("0.1.1"), true, None)), "0.1.0"),
            AboutCheck::Available {
                latest: "0.1.1".into()
            }
        );
        assert!(matches!(
            about_check_from_status(Some(&status(None, false, Some("offline"))), "0.1.0"),
            AboutCheck::Failed { .. }
        ));
        assert_eq!(
            install_kind_label(&cypher_update::InstallKind::Unmanaged),
            "Development build"
        );
    }

    #[test]
    fn update_strip_presentation() {
        use cypher_update::UpdateStatus;
        // `engine_current` = the attached daemon's compiled version (which can
        // differ from the UI process's), `app` = the version running in THIS
        // process.
        let status = |engine_current: &str, latest: Option<&str>, available: bool| UpdateStatus {
            current_version: engine_current.into(),
            latest_version: latest.map(str::to_string),
            update_available: available,
            checked_at: None,
            error: None,
            relaunch_pending: false,
        };
        // App-version source of truth: both table cases are decided against the
        // UI process's version, never the engine's `update_available` boolean.
        let mac_visible = |engine_current: &str, available: bool, latest: &str, app: &str| {
            update_strip_view(
                Some(&status(engine_current, Some(latest), available)),
                app,
                None,
                true,
                &UpdateFlow::Idle,
            )
            .is_some()
        };

        // Table: app 0.1.0 + engine 0.1.1/latest 0.1.1 → SHOW. The engine is
        // up to date (its boolean is false) but this UI process is older.
        assert!(mac_visible("0.1.1", false, "0.1.1", "0.1.0"));
        // Table: app 0.1.1 + old engine boolean true/latest 0.1.1 → HIDE. The
        // engine daemon is merely older; this UI process is already at latest.
        assert!(!mac_visible("0.1.0", true, "0.1.1", "0.1.1"));
        // Same version from either side never shows.
        assert!(!mac_visible("0.1.1", true, "0.1.1", "0.1.1"));
        assert!(!mac_visible("0.1.0", false, "0.1.1", "0.1.1"));
        // A genuinely newer release than this process still shows, whatever the
        // engine says.
        assert!(mac_visible("0.1.1", false, "0.1.2", "0.1.1"));

        // Hidden: no status, no latest, or the dismissed version.
        assert!(update_strip_view(None, "0.1.0", None, true, &UpdateFlow::Idle).is_none());
        assert!(
            update_strip_view(
                Some(&status("0.1.0", None, false)),
                "0.1.0",
                None,
                true,
                &UpdateFlow::Idle
            )
            .is_none()
        );
        assert!(
            update_strip_view(
                Some(&status("0.1.0", Some("0.1.1"), true)),
                "0.1.0",
                Some("0.1.1"),
                true,
                &UpdateFlow::Idle
            )
            .is_none()
        );
        // A newer release than the dismissed one shows again.
        assert!(
            update_strip_view(
                Some(&status("0.1.0", Some("0.1.2"), true)),
                "0.1.0",
                Some("0.1.1"),
                true,
                &UpdateFlow::Idle
            )
            .is_some()
        );

        // Advisory (non-mac) install also follows this process's version, not
        // the attached engine's boolean.
        let view = update_strip_view(
            Some(&status("0.1.1", Some("0.1.1"), false)),
            "0.1.0",
            None,
            false,
            &UpdateFlow::Idle,
        )
        .unwrap();
        assert_eq!(
            view.label,
            "Update available — v0.1.1 · run `cypher update`"
        );
        assert!(view.clickable && !view.failed);
        assert!(
            update_strip_view(
                Some(&status("0.1.0", Some("0.1.1"), true)),
                "0.1.1",
                None,
                false,
                &UpdateFlow::Idle,
            )
            .is_none()
        );

        // Mac bundle flow: Idle / Downloading / Ready / Failed labels + flags.
        let view = update_strip_view(
            Some(&status("0.1.0", Some("0.1.1"), true)),
            "0.1.0",
            None,
            true,
            &UpdateFlow::Idle,
        )
        .unwrap();
        assert_eq!(view.label, "Update available — v0.1.1");
        assert!(view.clickable && !view.failed);

        let view = update_strip_view(
            Some(&status("0.1.0", Some("0.1.1"), true)),
            "0.1.0",
            None,
            true,
            &UpdateFlow::Downloading,
        )
        .unwrap();
        assert_eq!(view.label, "Downloading v0.1.1…");
        assert!(!view.clickable && !view.failed);

        let view = update_strip_view(
            Some(&status("0.1.0", Some("0.1.1"), true)),
            "0.1.0",
            None,
            true,
            &UpdateFlow::Ready(PathBuf::from("/tmp/staged")),
        )
        .unwrap();
        assert_eq!(view.label, "Update ready — restart to apply");
        assert!(view.clickable && !view.failed);

        let view = update_strip_view(
            Some(&status("0.1.0", Some("0.1.1"), true)),
            "0.1.0",
            None,
            true,
            &UpdateFlow::Failed("checksum mismatch".into()),
        )
        .unwrap();
        assert_eq!(view.label, "Update failed: checksum mismatch");
        assert!(view.clickable && view.failed);
    }

    #[test]
    fn pi_update_strip_presentation() {
        use cypher_engine::pi_packages::{PiPackageUpdate, PiUpdateStatus};

        let status =
            |pi: bool, packages: usize, applying: bool, error: Option<&str>| PiUpdateStatus {
                pi_installed: true,
                current_pi_version: Some("0.84.0".into()),
                latest_pi_version: Some("0.85.0".into()),
                pi_update_available: pi,
                package_updates: (0..packages)
                    .map(|index| PiPackageUpdate {
                        name: format!("plugin-{index}"),
                        current_version: "1.0.0".into(),
                        latest_version: "1.1.0".into(),
                    })
                    .collect(),
                applying,
                checked_at: None,
                error: error.map(str::to_string),
            };

        assert!(pi_update_strip_view(None, false).is_none());
        assert!(pi_update_strip_view(Some(&status(false, 0, false, None)), false).is_none());

        let pi_only = pi_update_strip_view(Some(&status(true, 0, false, None)), false).unwrap();
        assert_eq!(pi_only.label, "Pi update available — v0.85.0");
        assert!(pi_only.clickable && !pi_only.failed);

        let combined = pi_update_strip_view(Some(&status(true, 2, false, None)), false).unwrap();
        assert_eq!(combined.label, "Pi and 2 plugin updates available");

        let busy = pi_update_strip_view(Some(&status(false, 1, true, None)), false).unwrap();
        assert_eq!(busy.label, "Updating Pi and plugins…");
        assert!(!busy.clickable);

        let failed =
            pi_update_strip_view(Some(&status(false, 1, false, Some("network"))), false).unwrap();
        assert!(failed.failed && failed.clickable);
    }

    #[test]
    fn about_runtime_line_reports_the_manual_runtime_check() {
        use cypher_engine::pi_packages::{PiPackageUpdate, PiUpdateStatus};

        let status =
            |pi: bool, packages: usize, applying: bool, error: Option<&str>| PiUpdateStatus {
                pi_installed: true,
                current_pi_version: Some("0.85.1".into()),
                latest_pi_version: Some("0.85.2".into()),
                pi_update_available: pi,
                package_updates: (0..packages)
                    .map(|index| PiPackageUpdate {
                        name: format!("plugin-{index}"),
                        current_version: "1.0.0".into(),
                        latest_version: "1.1.0".into(),
                    })
                    .collect(),
                applying,
                checked_at: None,
                error: error.map(str::to_string),
            };

        // No Runtime facts yet, or no Runtime at all: the dialog stays quiet.
        assert!(about_runtime_line(None, true).is_none());
        let missing = PiUpdateStatus {
            pi_installed: false,
            ..status(false, 0, false, None)
        };
        assert!(about_runtime_line(Some(&missing), false).is_none());

        assert_eq!(
            about_runtime_line(Some(&status(false, 0, false, None)), true).unwrap(),
            "Checking the Pi Runtime…"
        );
        assert_eq!(
            about_runtime_line(Some(&status(false, 0, false, None)), false).unwrap(),
            "Pi Runtime is up to date."
        );
        // An install found by the check outranks the checking label: it is the
        // newer, more specific truth about the same sweep.
        assert_eq!(
            about_runtime_line(Some(&status(false, 1, true, None)), true).unwrap(),
            "Updating the Pi Runtime…"
        );
        assert_eq!(
            about_runtime_line(Some(&status(false, 1, false, None)), false).unwrap(),
            "Pi Runtime update available — 1 plugin."
        );
        assert_eq!(
            about_runtime_line(Some(&status(false, 3, false, None)), false).unwrap(),
            "Pi Runtime update available — 3 plugins."
        );
        assert_eq!(
            about_runtime_line(Some(&status(true, 0, false, None)), false).unwrap(),
            "Pi Runtime update available — Pi 0.85.2"
        );
        assert_eq!(
            about_runtime_line(Some(&status(false, 0, false, Some("offline"))), false).unwrap(),
            "Pi Runtime check failed: offline"
        );
    }

    #[tokio::test]
    async fn signed_out_synced_runtime_stops_and_reboots_local() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session.json"),
            r#"{"refreshToken":"still-valid","user":{"id":"user_1","email":"u@example.com"},"orgId":"org_1"}"#,
        )
        .unwrap();
        let port = cypher_env::ipc_socket(dir.path()).unwrap();
        let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
        drop(listener);
        let boot = EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_socket: port.clone(),
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: Some("client_test".into()),
            default_harness: cypher_proto::HarnessId::Mock,
        };
        let synced = crate::state::EngineHandle::bootstrap(boot.clone())
            .await
            .expect("saved session opens its synced profile");
        assert_eq!(synced.engine_info().workspace_scope, WorkspaceScope::Synced);

        synced
            .client()
            .call(methods::SIGN_OUT, serde_json::json!({}))
            .await
            .expect("sign out clears credentials");
        stop_synced_runtime(synced, port, dir.path())
            .await
            .expect("synced runtime drains and releases ownership");

        assert!(!dir.path().join("session.json").exists());
        let local = crate::state::EngineHandle::bootstrap(boot)
            .await
            .expect("same process can continue locally");
        assert_eq!(local.engine_info().workspace_scope, WorkspaceScope::Local);
        local.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_shutdown_waits_for_engine_lock_release() {
        let dir = tempfile::tempdir().unwrap();
        let lock = InstanceLock::acquire(dir.path()).unwrap();
        let port = cypher_env::ipc_socket(dir.path()).unwrap();
        let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
        drop(listener);
        let lock_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let released_by_task = lock_released.clone();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(lock);
            released_by_task.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(lock_released.load(std::sync::atomic::Ordering::SeqCst));
        release.await.unwrap();
    }

    #[tokio::test]
    async fn remote_shutdown_times_out_while_ipc_remains_open() {
        let dir = tempfile::tempdir().unwrap();
        let port = cypher_env::ipc_socket(dir.path()).unwrap();
        let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();

        let error = wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_millis(100))
            .await
            .unwrap_err();

        assert!(error.contains("did not finish stopping"));
        drop(listener);
    }

    #[test]
    fn account_actions_follow_the_attached_workspace_scope() {
        assert_eq!(
            account_menu_action(Some(WorkspaceScope::Local), SyncFlow::Idle),
            Some(AccountMenuAction::EnableSync)
        );
        assert_eq!(
            account_menu_action(Some(WorkspaceScope::Synced), SyncFlow::Idle),
            Some(AccountMenuAction::SignOut)
        );
        assert_eq!(
            account_menu_action(Some(WorkspaceScope::Development), SyncFlow::Idle),
            None
        );
    }

    #[test]
    fn local_sign_in_offers_the_in_place_switch() {
        let signed_in = AuthState::SignedIn {
            user: cypher_proto::UserProfile {
                id: "user-1".into(),
                email: "user@example.com".into(),
                name: None,
                avatar_url: None,
            },
            org_id: Some("org-1".into()),
        };

        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::Enabling,
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::SwitchOffer { notice_open: true }
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::Idle,
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::SwitchOffer { notice_open: true },
            "another viewport derives the pending switch from AuthStatus"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SwitchOffer { notice_open: false },
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::SwitchOffer { notice_open: false },
            "shared auth updates do not reopen a postponed wizard"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::RestartPending { notice_open: false },
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::RestartPending { notice_open: false },
            "the quit fallback survives shared auth updates too"
        );
        assert_eq!(
            account_menu_action(
                Some(WorkspaceScope::Local),
                SyncFlow::SwitchOffer { notice_open: false },
            ),
            Some(AccountMenuAction::RestartPending)
        );
        for notice_open in [true, false] {
            assert_eq!(
                sync_flow_after_auth(
                    SyncFlow::SwitchOffer { notice_open },
                    Some(WorkspaceScope::Local),
                    Some(&AuthState::SignedOut),
                ),
                SyncFlow::Idle,
                "revoked credentials cancel the pending switch"
            );
        }
    }

    #[test]
    fn import_summary_errors_are_a_failure_not_a_success() {
        // Clean summary → done with counts.
        let clean = serde_json::json!({
            "kind": "summary", "importedChats": 2, "skippedChats": 1, "errors": []
        });
        assert_eq!(import_summary_outcome(&clean), Ok((2, 1)));

        // Any error means the wizard must NOT say "all set" — partial
        // migrations surface as an explicit failure with the first cause.
        let partial = serde_json::json!({
            "kind": "summary", "importedChats": 1, "skippedChats": 0,
            "errors": ["chat c2: journal copy failed"]
        });
        let message = import_summary_outcome(&partial).expect_err("errors must fail");
        assert!(message.contains("journal copy failed"), "{message}");
        assert!(message.contains("1 imported"), "{message}");

        let many = serde_json::json!({
            "kind": "summary", "importedChats": 0, "skippedChats": 0,
            "errors": ["a", "b", "c"]
        });
        let message = import_summary_outcome(&many).expect_err("errors must fail");
        assert!(message.contains("3 failures"), "{message}");

        // A summary missing the errors field entirely (older engine) is
        // treated as clean rather than failing every import.
        let legacy = serde_json::json!({ "kind": "summary", "importedChats": 4 });
        assert_eq!(import_summary_outcome(&legacy), Ok((4, 0)));
    }

    #[test]
    fn spaces_only_local_work_still_gets_the_import_offer() {
        assert_eq!(local_work_phrase(0, 0), None, "nothing to bring");
        assert_eq!(local_work_phrase(2, 0).as_deref(), Some("the 2 sessions"));
        assert_eq!(
            local_work_phrase(0, 1).as_deref(),
            Some("the 1 project"),
            "a projects-only profile must be offered the import, not a bare switch"
        );
        assert_eq!(
            local_work_phrase(1, 2).as_deref(),
            Some("the 1 session and 2 projects")
        );
    }

    #[test]
    fn dismissed_import_failure_stays_reachable_on_a_synced_runtime() {
        let signed_in = AuthState::SignedIn {
            user: cypher_proto::UserProfile {
                id: "user-1".into(),
                email: "user@example.com".into(),
                name: None,
                avatar_url: None,
            },
            org_id: Some("org-1".into()),
        };

        // "Later" postpones the failure notice; it must not evaporate.
        let dismissed = SyncFlow::ImportFailed { notice_open: false };
        assert_eq!(
            sync_flow_after_auth(dismissed, Some(WorkspaceScope::Synced), Some(&signed_in)),
            dismissed,
            "a postponed import failure survives auth/scope updates"
        );

        // …and the account menu on the SYNCED runtime still exposes the
        // re-entry point. This is the whole point: after the switch there is
        // no local runtime left to re-derive an offer from, so this menu row
        // is the only path back to the retry dialog.
        assert_eq!(
            account_menu_action(Some(WorkspaceScope::Synced), dismissed),
            Some(AccountMenuAction::RestartPending),
            "retry must remain reachable after dismissal"
        );
        assert_eq!(
            account_menu_action(
                Some(WorkspaceScope::Synced),
                SyncFlow::ImportFailed { notice_open: true },
            ),
            Some(AccountMenuAction::RestartPending)
        );

        // Resolving the failure restores the normal synced menu.
        assert_eq!(
            account_menu_action(Some(WorkspaceScope::Synced), SyncFlow::Idle),
            Some(AccountMenuAction::SignOut)
        );
    }

    #[test]
    fn switch_lifecycle_survives_the_runtime_replacement_window() {
        let signed_in = AuthState::SignedIn {
            user: cypher_proto::UserProfile {
                id: "user-1".into(),
                email: "user@example.com".into(),
                name: None,
                avatar_url: None,
            },
            org_id: Some("org-1".into()),
        };
        for flow in [
            SyncFlow::Switching { import: true },
            SyncFlow::Importing { done: 1, total: 3 },
            SyncFlow::ImportDone {
                imported: 3,
                skipped: 0,
            },
            SyncFlow::ImportFailed { notice_open: true },
            SyncFlow::ImportFailed { notice_open: false },
        ] {
            // Local (before the stop), detached (mid-replacement), and synced
            // (replacement runtime up): the driver owns these states — auth
            // and scope edges must never reset them.
            assert_eq!(
                sync_flow_after_auth(flow, Some(WorkspaceScope::Local), Some(&signed_in)),
                flow
            );
            assert_eq!(sync_flow_after_auth(flow, None, None), flow);
            assert_eq!(
                sync_flow_after_auth(flow, Some(WorkspaceScope::Synced), Some(&signed_in)),
                flow
            );
        }
    }

    #[test]
    fn synced_sign_out_blocks_every_viewport_and_cannot_switch_accounts() {
        let signed_in_as_another_user = AuthState::SignedIn {
            user: cypher_proto::UserProfile {
                id: "user-2".into(),
                email: "other@example.com".into(),
                name: None,
                avatar_url: None,
            },
            org_id: Some("org-2".into()),
        };

        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SigningOut,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::SignedOut),
            ),
            SyncFlow::SignedOutRestartRequired,
            "the viewport that requested sign-out is blocked by AuthStatus"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::Idle,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::SignedOut),
            ),
            SyncFlow::SignedOutRestartRequired,
            "another viewport observing the same runtime is also blocked"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SignedOutRestartRequired,
                Some(WorkspaceScope::Synced),
                Some(&signed_in_as_another_user),
            ),
            SyncFlow::SignedOutRestartRequired,
            "new credentials cannot reopen the previous account's store"
        );
    }

    #[test]
    fn titlebar_cluster_matches_cypher_window_controls() {
        // zeron window-controls.tsx: `left: fullscreen ? 12 : 88` — the
        // cluster clears the {14,15} traffic lights, and reclaims the inset
        // when fullscreen hides them.
        assert_eq!(titlebar_cluster_start(false), 88.0);
        assert_eq!(titlebar_cluster_start(true), 12.0);
    }

    #[test]
    fn windows_caption_controls_reserve_titlebar_space() {
        assert_eq!(titlebar_right_padding(true, 16.0), 124.0);
        assert_eq!(titlebar_right_padding(false, 16.0), 16.0);
    }

    #[test]
    fn cluster_buttons_start_per_platform() {
        // Linux: buttons at 10..86.
        assert_eq!(cluster_buttons_start(false, false), 10.0);
        assert_eq!(CLUSTER_BUTTONS_WIDTH, 76.0);
        // macOS: buttons start at the 88px traffic-light cluster start…
        assert_eq!(cluster_buttons_start(true, false), 88.0);
        // …and reclaim the inset in fullscreen (starts at 12).
        assert_eq!(cluster_buttons_start(true, true), 12.0);
    }

    // ---- sidebar resort FLIP diff (§1.6) ----

    fn keys(list: &[(&str, f32)]) -> Vec<(String, f32)> {
        list.iter().map(|(k, h)| (k.to_string(), *h)).collect()
    }

    #[test]
    fn resort_offsets_empty_when_order_unchanged() {
        let order = keys(&[("a", 29.0), ("b", 29.0), ("c", 45.0)]);
        assert!(resort_offsets(&order, &order, 2.0).is_empty());
    }

    #[test]
    fn resort_offsets_activity_moves_row_to_top() {
        // c (bottom, y=62) jumps to top: c glides down-from-above? No — c's
        // old y is 62, new y is 0 → starts +62 below… offset = old - new = +62,
        // painted at +62 decaying to 0 (a glide UP into place). a and b shift
        // down by c's height + gap (31).
        let old = keys(&[("a", 29.0), ("b", 29.0), ("c", 29.0)]);
        let new = keys(&[("c", 29.0), ("a", 29.0), ("b", 29.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        assert_eq!(offsets.get("c"), Some(&62.0));
        assert_eq!(offsets.get("a"), Some(&-31.0));
        assert_eq!(offsets.get("b"), Some(&-31.0));
    }

    #[test]
    fn resort_offsets_respect_heights_and_gap() {
        // Tall row (45px) swaps with a short one (29px).
        let old = keys(&[("tall", 45.0), ("short", 29.0)]);
        let new = keys(&[("short", 29.0), ("tall", 45.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        // short: old y 47 → new y 0; tall: old y 0 → new y 31.
        assert_eq!(offsets.get("short"), Some(&47.0));
        assert_eq!(offsets.get("tall"), Some(&-31.0));
    }

    #[test]
    fn resort_offsets_ignore_added_and_removed_keys() {
        let old = keys(&[("a", 29.0), ("gone", 29.0), ("b", 29.0)]);
        let new = keys(&[("new", 29.0), ("a", 29.0), ("b", 29.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        // "new" has no old position (fades in instead); "gone" just goes.
        assert!(!offsets.contains_key("new"));
        assert!(!offsets.contains_key("gone"));
        // a: old 0 → new 31 (pushed down by the insert); b: 62 → 62 (gone's
        // slot replaced by "new" of equal height — no move, no entry).
        assert_eq!(offsets.get("a"), Some(&-31.0));
        assert_eq!(offsets.get("b"), None);
    }

    #[test]
    fn resort_glide_spec_matches_original() {
        // §1.6: 260ms cubic-bezier(0.22, 1, 0.36, 1).
        assert_eq!(RESORT.duration_ms, 260);
        assert_eq!(RESORT.curve, motion::EASE_RESORT);
    }

    // ---- navigation history (titlebar back/forward) ----

    fn chat(id: &str) -> NavEntry {
        NavEntry::Chat(id.to_string())
    }

    #[test]
    fn settings_navigation_groups_are_disjoint_and_complete() {
        let listed = SettingsSection::NAV_GROUPS
            .iter()
            .flat_map(|(_, sections)| sections.iter().copied())
            .collect::<Vec<_>>();
        assert_eq!(listed.len(), SettingsSection::ALL.len());
        for section in SettingsSection::ALL {
            assert_eq!(
                listed.iter().filter(|s| **s == section).count(),
                1,
                "{section:?}"
            );
        }
        assert_eq!(
            SettingsSection::DEVICE_SETTINGS,
            [
                SettingsSection::Harnesses,
                SettingsSection::Providers,
                SettingsSection::Subagents,
                SettingsSection::Commands,
                SettingsSection::Mcp,
                SettingsSection::Github,
            ]
        );
        assert_eq!(
            SettingsSection::CLIENT_SETTINGS,
            [
                SettingsSection::Appearance,
                SettingsSection::Notifications,
                SettingsSection::Shortcuts,
            ]
        );
        assert_eq!(
            SettingsSection::WORKSPACE_SETTINGS,
            [SettingsSection::Devices, SettingsSection::Archived,]
        );
    }

    #[test]
    fn nav_history_starts_with_nothing_to_walk() {
        let nav = NavHistory::new(chat(""));
        assert!(!nav.can_back());
        assert!(!nav.can_forward());
        assert_eq!(*nav.current(), chat(""));
    }

    #[test]
    fn nav_push_then_back_and_forward() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("b"));
        nav.push(NavEntry::Settings(SettingsSection::Devices));
        assert!(nav.can_back());
        assert!(!nav.can_forward());

        // Back walks toward the oldest entry without dropping anything.
        assert_eq!(
            nav.back(),
            Some(chat("b")),
            "back lands on the previous route"
        );
        assert_eq!(nav.back(), Some(chat("a")));
        assert!(!nav.can_back());
        assert!(nav.can_forward());
        assert_eq!(nav.back(), None, "past the oldest entry is a no-op");

        // Forward retraces the same path.
        assert_eq!(nav.forward(), Some(chat("b")));
        assert_eq!(
            nav.forward(),
            Some(NavEntry::Settings(SettingsSection::Devices))
        );
        assert!(!nav.can_forward());
        assert_eq!(nav.forward(), None);
    }

    #[test]
    fn nav_push_dedups_the_current_route() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("a"));
        nav.push(chat("a"));
        assert_eq!(nav.len(), 1, "re-selecting the current route never stacks");
        nav.push(NavEntry::Settings(SettingsSection::Agents));
        nav.push(NavEntry::Settings(SettingsSection::Agents));
        assert_eq!(nav.len(), 2);
    }

    #[test]
    fn nav_canvas_push_from_a_chat_is_one_entry_and_dedups_on_repeat() {
        // `open_new_session` / `land_in_space` push the empty canvas route
        // explicitly; `on_state_changed` follows with the same entry after
        // the chat clears. Both must land as ONE entry (the push dedups),
        // and a second `+` while already on the canvas must not stack either.
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("")); // open_new_session: chat "a" → canvas
        nav.push(chat("")); // on_state_changed after select_chat(None)
        nav.push(chat("")); // another `+` on the canvas
        assert_eq!(nav.len(), 2, "canvas push dedups against itself");
        assert_eq!(*nav.current(), chat(""));
        assert_eq!(nav.back(), Some(chat("a")), "Back returns to the session");

        // Already on the canvas: pushing the canvas is a strict no-op.
        let mut again = NavHistory::new(chat(""));
        again.push(chat(""));
        assert_eq!(again.len(), 1);
        assert!(!again.can_back());
    }

    #[test]
    fn nav_push_truncates_the_forward_branch() {
        // a → b → c, back to a, then push d: the b/c branch is gone (browser
        // semantics — zeron's memory history PUSH truncates entries ahead).
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("b"));
        nav.push(chat("c"));
        nav.back();
        nav.back();
        assert_eq!(*nav.current(), chat("a"));
        assert!(nav.can_forward());
        nav.push(chat("d"));
        assert!(!nav.can_forward(), "the old branch is unreachable");
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.back(), Some(chat("a")));
        assert_eq!(nav.forward(), Some(chat("d")));
    }

    #[test]
    fn nav_replace_swaps_in_place() {
        // The boot auto-select replaces the untouched canvas entry, so Back
        // stays disabled after landing in the last-used chat.
        let mut nav = NavHistory::new(chat(""));
        nav.replace(chat("boot"));
        assert_eq!(nav.len(), 1);
        assert_eq!(*nav.current(), chat("boot"));
        assert!(!nav.can_back());
    }

    #[test]
    fn nav_settings_sections_are_distinct_entries() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(NavEntry::Settings(SettingsSection::Devices));
        nav.push(NavEntry::Settings(SettingsSection::Shortcuts));
        assert_eq!(nav.len(), 3, "section changes are navigations");
        assert_eq!(
            nav.back(),
            Some(NavEntry::Settings(SettingsSection::Devices))
        );
        assert_eq!(nav.back(), Some(chat("a")));
    }
}
