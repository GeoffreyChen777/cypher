//! UI settings persisted to a small JSON file in the data dir — pane widths and
//! collapse flags (zeron persisted the same set in localStorage).
//!
//! Loaded once at boot; saved debounced by the shell ([`SAVE_DEBOUNCE_MS`]).
//! Corrupt or missing files fall back to defaults; loaded values are clamped so a
//! hand-edited file can't wedge the layout.

use std::io;
use std::path::{Path, PathBuf};

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod accounts;
pub mod appearance;
pub mod archived;
pub mod commands;
pub mod composer;
pub mod device_target;
pub mod devices;
pub mod github;
pub mod harnesses;
pub mod mcp;
pub mod notifications;
pub mod providers;
pub mod setup;
pub mod shortcuts;
pub mod subagents;
pub mod titles;
pub mod translation;
pub mod web_search;
pub mod widgets;

/// Sidebar drag-resize bounds (px).
pub const SIDEBAR_MIN: f32 = 208.0;
pub const SIDEBAR_MAX: f32 = 400.0;
pub const SIDEBAR_DEFAULT: f32 = 256.0;

/// Right ("Changes") pane drag-resize bounds (px).
pub const RIGHT_PANE_MIN: f32 = 360.0;
pub const RIGHT_PANE_MAX: f32 = 1368.0;
pub const RIGHT_PANE_DEFAULT: f32 = 520.0;

/// Terminal panel height bounds: 160px … 55% of the viewport (§1.10). The
/// viewport-relative cap applies at runtime; the absolute cap here only heals
/// hand-edited files.
pub const TERMINAL_MIN_HEIGHT: f32 = 160.0;
pub const TERMINAL_MAX_VH: f32 = 0.55;
pub const TERMINAL_ABS_MAX_HEIGHT: f32 = 2000.0;
pub const TERMINAL_DEFAULT_HEIGHT: f32 = 280.0;

/// Sanity range for a persisted dock fraction (share of the session area).
/// The pixel clamps (chat column, dock and terminal minimums) apply at
/// render time; this only heals hand-edited files.
pub const DOCK_FRACTION_MIN: f32 = 0.05;
pub const DOCK_FRACTION_MAX: f32 = 0.95;

/// At most this many sessions remember their dock sizes; the least recently
/// changed entries go first.
pub const SESSION_DOCKS_CAP: usize = 200;

/// Debounce for settings writes after a drag/toggle.
pub const SAVE_DEBOUNCE_MS: u64 = 400;

const FILE_NAME: &str = "ui-settings.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiSettings {
    pub sidebar_width: f32,
    pub sidebar_collapsed: bool,
    /// Legacy: the grouped-by-project toggle predates spaces (which group by
    /// folder inherently). Kept for file compatibility; no longer read.
    pub sidebar_grouped: bool,
    /// The last selected space — restored on boot when the row still exists;
    /// also the new-session canvas's default target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_space_id: Option<String>,
    /// Open session tabs in visual order (drag-reorder edits in place).
    /// Device-local: a tab is a local viewport onto the synced session list —
    /// closing one never archives the session. Ids of archived/deleted chats
    /// are pruned against the doc ([`Shell::sync_open_tabs`]). `None` = file
    /// written by a pre-tabs build; seeded once from the last space's sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_tabs: Option<Vec<String>>,
    /// Legacy: the sidebar's project filter, from when a dropdown filtered
    /// the session list. Serialized as `spaceFilter` for file compatibility
    /// but ignored by current runtime behavior — the sidebar always shows
    /// every project now, and the canvas target selectors are the only
    /// switcher. It remains round-trippable for compatibility.
    #[serde(rename = "spaceFilter", skip_serializing_if = "Option::is_none")]
    pub legacy_space_filter: Option<String>,
    /// Legacy: per-space tab order, from when tabs were the selected space's
    /// non-archived sessions. Kept for file compatibility; no longer read.
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub tab_order: std::collections::HashMap<String, Vec<String>>,
    /// Legacy: manual sidebar space order, from when spaces were a sidebar
    /// list. Kept for file compatibility; no longer read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_order: Vec<String>,
    /// Session notification chimes (done / awaiting-input). `CYPHER_DISABLE_SOUND`
    /// overrides.
    pub sound_enabled: bool,
    /// Desktop banner notifications on the same transitions.
    /// `CYPHER_DISABLE_NOTIFICATIONS` overrides.
    pub notifications_enabled: bool,
    /// Suppress the banner while a Cypher window is focused (the chime covers
    /// the foreground case).
    pub notifications_background_only: bool,
    /// Unread count on the Dock icon: sessions waiting on input, errored or
    /// finished-unseen (`AppState::attention_count`).
    pub dock_badge_enabled: bool,
    /// Legacy global right dock width (px), from before per-session docks:
    /// no longer written; the size of a session whose dock was never dragged
    /// (see [`SessionDock`]).
    pub right_pane_width: f32,
    /// Legacy: panel *open* flags are session-scoped in-memory state now
    /// (`shell::SessionPanels`, zeron `sessionPanels` parity). Kept for file
    /// compatibility; no longer read or written by the shell.
    pub right_pane_open: bool,
    /// Legacy global terminal height (px) — see [`Self::right_pane_width`].
    pub terminal_height: f32,
    /// Legacy — see [`Self::right_pane_open`].
    pub terminal_open: bool,
    /// Customizable shortcut combos (feature-inventory §1.4).
    pub keymap: KeymapConfig,
    /// Light/dark preference. Defaults to following the OS.
    pub appearance: crate::appearance::AppearanceMode,
    /// First-run Pi/extensions setup has been dismissed. Missing on files
    /// written before this screen existed — those load as `false` so the
    /// setup still appears once.
    pub setup_completed: bool,
    /// Versioned setup contract. Version 1 introduces Cypher's downloaded,
    /// system-independent Pi runtime; pre-runtime settings deserialize as 0
    /// and receive the one-time download prompt.
    pub pi_runtime_setup_version: u32,
    /// Slash commands the user turned on for the composer `/` menu; every
    /// other command stays hidden ([`commands::ShownSlashCommands`]). Older
    /// files kept a `hiddenSlashCommands` list instead, which held nothing a
    /// user had turned on, so it is ignored and dropped on the next save.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shown_slash_commands: Vec<String>,
    /// Commands [`commands::SHOWN_BY_DEFAULT`] has already turned on, so a
    /// user who turns one off keeps it off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offered_slash_commands: Vec<String>,
    /// Sidebar card order (the header's view menu). Pins always lead.
    pub sidebar_sort: SidebarSort,
    /// Sidebar device filter: only cards hosted on this device id. `None`
    /// shows every device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_device_filter: Option<String>,
    /// Sidebar sort direction flipped from the sort's natural one (newest
    /// first for activity/date, A→Z for name/device).
    pub sidebar_sort_reversed: bool,
    /// The main window's workspace layout (docs/workspace-layout.md,
    /// decision 1). Deserialization repairs it; the shell prunes tabs of
    /// chats that no longer exist once the first chats frame lands.
    /// These three load leniently ([`lenient`], [`lenient_map`]): an
    /// unreadable layout (a downgrade's unknown tab kind, a hand edit) is
    /// dropped on its own instead of resetting every setting.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace: Option<crate::workspace::Workspace>,
    /// Each project window's own layout, keyed by project (space) id.
    #[serde(
        default,
        deserialize_with = "lenient_map",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub project_workspaces: HashMap<String, crate::workspace::Workspace>,
    /// Per-session dock sizes and open flags, keyed by chat id (decision 7).
    /// Bounded: see [`Self::remember_session_dock`].
    #[serde(
        default,
        deserialize_with = "lenient_map",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub session_docks: HashMap<String, SessionDock>,
}

/// One session's docks: sizes as fractions of its session area (so a tile
/// keeps its proportions when the layout changes), open flags, and when it
/// last changed. A `None` size has never been dragged and falls back to the
/// legacy global pixel size ([`UiSettings::right_pane_width`] /
/// [`UiSettings::terminal_height`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionDock {
    /// Right dock width / session area width.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<f32>,
    /// Terminal dock height / session area height.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<f32>,
    pub right_open: bool,
    pub terminal_open: bool,
    /// Unix milliseconds of the last change: picks the most recent entry
    /// (a new session's default sizes) and the entries the cap evicts.
    pub used_at: i64,
}

impl SessionDock {
    /// The defaults for a session without an entry: the most recently used
    /// sizes, docks closed (opening is always an explicit act).
    pub fn seeded_from(latest: Option<&SessionDock>) -> Self {
        Self {
            right: latest.and_then(|d| d.right),
            terminal: latest.and_then(|d| d.terminal),
            ..Self::default()
        }
    }

    /// Heal a loaded entry: fractions outside the sane range are clamped,
    /// non-finite ones dropped (back to the legacy default).
    pub fn clamped(mut self) -> Self {
        self.right = self.right.and_then(clamp_fraction);
        self.terminal = self.terminal.and_then(clamp_fraction);
        self
    }
}

/// A dock fraction clamped to the sane range; `None` when not finite.
pub fn clamp_fraction(fraction: f32) -> Option<f32> {
    fraction
        .is_finite()
        .then(|| fraction.clamp(DOCK_FRACTION_MIN, DOCK_FRACTION_MAX))
}

/// How the sidebar orders its project cards (and the sessions inside them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarSort {
    /// Newest activity first (the default).
    #[default]
    Activity,
    /// Project display name, A→Z; sessions by title.
    Name,
    /// Host device name, A→Z; recency within a device.
    Device,
    /// Creation date, newest first.
    Date,
}

impl SidebarSort {
    pub const ALL: [SidebarSort; 4] = [
        SidebarSort::Activity,
        SidebarSort::Name,
        SidebarSort::Device,
        SidebarSort::Date,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SidebarSort::Activity => "New activity",
            SidebarSort::Name => "Name",
            SidebarSort::Device => "Device",
            SidebarSort::Date => "Date created",
        }
    }

    /// The direction a sort reads as "ascending" is not what people want by
    /// default for time: activity and date lead with the newest.
    pub fn natural_descending(self) -> bool {
        matches!(self, SidebarSort::Activity | SidebarSort::Date)
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            sidebar_width: SIDEBAR_DEFAULT,
            sidebar_collapsed: false,
            sidebar_grouped: false,
            last_space_id: None,
            open_tabs: None,
            legacy_space_filter: None,
            tab_order: std::collections::HashMap::new(),
            space_order: Vec::new(),
            sound_enabled: true,
            notifications_enabled: true,
            notifications_background_only: true,
            dock_badge_enabled: true,
            right_pane_width: RIGHT_PANE_DEFAULT,
            right_pane_open: false,
            terminal_height: TERMINAL_DEFAULT_HEIGHT,
            terminal_open: false,
            keymap: KeymapConfig::default(),
            appearance: crate::appearance::AppearanceMode::default(),
            setup_completed: false,
            pi_runtime_setup_version: 0,
            shown_slash_commands: commands::SHOWN_BY_DEFAULT
                .iter()
                .map(|name| name.to_string())
                .collect(),
            offered_slash_commands: commands::SHOWN_BY_DEFAULT
                .iter()
                .map(|name| name.to_string())
                .collect(),
            sidebar_sort: SidebarSort::Activity,
            sidebar_device_filter: None,
            sidebar_sort_reversed: false,
            workspace: None,
            project_workspaces: HashMap::new(),
            session_docks: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Keymap (customizable shortcuts, §1.4)
// ---------------------------------------------------------------------------

/// The rebindable app shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutId {
    ToggleSidebar,
    ToggleChanges,
    ToggleTerminal,
    NewSession,
    NextSession,
    PrevSession,
    // Workspace layout (docs/workspace-layout.md).
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    CloseTab,
    ToggleZoom,
}

/// Rows of the Settings → Shortcuts table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutGroup {
    General,
    Workspace,
}

impl ShortcutGroup {
    pub const ALL: [ShortcutGroup; 2] = [ShortcutGroup::General, ShortcutGroup::Workspace];

    pub fn label(self) -> &'static str {
        match self {
            ShortcutGroup::General => "General",
            ShortcutGroup::Workspace => "Workspace",
        }
    }
}

impl ShortcutId {
    pub const ALL: [ShortcutId; 14] = [
        ShortcutId::ToggleSidebar,
        ShortcutId::ToggleChanges,
        ShortcutId::ToggleTerminal,
        ShortcutId::NewSession,
        ShortcutId::NextSession,
        ShortcutId::PrevSession,
        ShortcutId::SplitRight,
        ShortcutId::SplitDown,
        ShortcutId::FocusLeft,
        ShortcutId::FocusRight,
        ShortcutId::FocusUp,
        ShortcutId::FocusDown,
        ShortcutId::CloseTab,
        ShortcutId::ToggleZoom,
    ];

    /// Row label (zeron lib/shortcuts.ts `SHORTCUT_DEFINITIONS`, verbatim;
    /// the workspace rows are ours).
    pub fn label(self) -> &'static str {
        match self {
            ShortcutId::ToggleSidebar => "Toggle left sidebar",
            ShortcutId::ToggleChanges => "Toggle right dock",
            ShortcutId::ToggleTerminal => "Toggle terminal",
            ShortcutId::NewSession => "New session",
            ShortcutId::NextSession => "Next session",
            ShortcutId::PrevSession => "Previous session",
            ShortcutId::SplitRight => "Split right",
            ShortcutId::SplitDown => "Split down",
            ShortcutId::FocusLeft => "Focus tile to the left",
            ShortcutId::FocusRight => "Focus tile to the right",
            ShortcutId::FocusUp => "Focus tile above",
            ShortcutId::FocusDown => "Focus tile below",
            ShortcutId::CloseTab => "Close tab",
            ShortcutId::ToggleZoom => "Zoom tile",
        }
    }

    pub fn group(self) -> ShortcutGroup {
        match self {
            ShortcutId::ToggleSidebar
            | ShortcutId::ToggleChanges
            | ShortcutId::ToggleTerminal
            | ShortcutId::NewSession
            | ShortcutId::NextSession
            | ShortcutId::PrevSession => ShortcutGroup::General,
            ShortcutId::SplitRight
            | ShortcutId::SplitDown
            | ShortcutId::FocusLeft
            | ShortcutId::FocusRight
            | ShortcutId::FocusUp
            | ShortcutId::FocusDown
            | ShortcutId::CloseTab
            | ShortcutId::ToggleZoom => ShortcutGroup::Workspace,
        }
    }

    pub fn default_combo(self) -> &'static str {
        self.default_combo_on(cfg!(target_os = "macos"))
    }

    /// `default_combo` for an explicit platform, so the spelling invariant is
    /// testable for both from any machine (see the tests below — the mismatch
    /// this guards against only exists off macOS).
    pub fn default_combo_on(self, mac: bool) -> &'static str {
        match self {
            ShortcutId::ToggleSidebar => "mod-s",
            ShortcutId::ToggleChanges => "mod-b",
            ShortcutId::ToggleTerminal => "mod-j",
            ShortcutId::NewSession => "mod-n",
            // Ctrl+Tab on every platform — but spelled the way THAT platform's
            // recorder spells ctrl (see `combo_from_keystroke`). Off macOS
            // ctrl IS the primary and stores as "mod"; on macOS it is its own
            // modifier, and "mod" would mean Cmd+Tab, which the OS app
            // switcher eats.
            //
            // Off macOS "ctrl-tab" and "mod-tab" resolve to the same keystroke
            // through `platform_combo`, but conflict detection compares the
            // STORED spelling — so a default the recorder cannot reproduce
            // would let a rebind onto that same physical key pass as
            // conflict-free, bind twice, and silently kill one shortcut.
            ShortcutId::NextSession if mac => "ctrl-tab",
            ShortcutId::NextSession => "mod-tab",
            ShortcutId::PrevSession if mac => "ctrl-shift-tab",
            ShortcutId::PrevSession => "mod-shift-tab",
            ShortcutId::SplitRight => "mod-\\",
            ShortcutId::SplitDown => "mod-shift-\\",
            ShortcutId::FocusLeft => "mod-alt-left",
            ShortcutId::FocusRight => "mod-alt-right",
            ShortcutId::FocusUp => "mod-alt-up",
            ShortcutId::FocusDown => "mod-alt-down",
            // ⌘W stays Close Window.
            ShortcutId::CloseTab => "mod-shift-w",
            ShortcutId::ToggleZoom => "mod-shift-enter",
        }
    }
}

/// Persisted shortcut combos. Stored platform-neutral ("mod-s"); translated to
/// "cmd-s"/"ctrl-s" at bind time by [`platform_combo`]. Missing fields (a file
/// from an older build) load their defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct KeymapConfig {
    pub toggle_sidebar: String,
    pub toggle_changes: String,
    pub toggle_terminal: String,
    pub new_session: String,
    pub next_session: String,
    pub prev_session: String,
    pub split_right: String,
    pub split_down: String,
    pub focus_left: String,
    pub focus_right: String,
    pub focus_up: String,
    pub focus_down: String,
    pub close_tab: String,
    pub toggle_zoom: String,
}

impl Default for KeymapConfig {
    fn default() -> Self {
        let mut keymap = Self {
            toggle_sidebar: String::new(),
            toggle_changes: String::new(),
            toggle_terminal: String::new(),
            new_session: String::new(),
            next_session: String::new(),
            prev_session: String::new(),
            split_right: String::new(),
            split_down: String::new(),
            focus_left: String::new(),
            focus_right: String::new(),
            focus_up: String::new(),
            focus_down: String::new(),
            close_tab: String::new(),
            toggle_zoom: String::new(),
        };
        for id in ShortcutId::ALL {
            keymap.reset(id);
        }
        keymap
    }
}

impl KeymapConfig {
    pub fn get(&self, id: ShortcutId) -> &str {
        match id {
            ShortcutId::ToggleSidebar => &self.toggle_sidebar,
            ShortcutId::ToggleChanges => &self.toggle_changes,
            ShortcutId::ToggleTerminal => &self.toggle_terminal,
            ShortcutId::NewSession => &self.new_session,
            ShortcutId::NextSession => &self.next_session,
            ShortcutId::PrevSession => &self.prev_session,
            ShortcutId::SplitRight => &self.split_right,
            ShortcutId::SplitDown => &self.split_down,
            ShortcutId::FocusLeft => &self.focus_left,
            ShortcutId::FocusRight => &self.focus_right,
            ShortcutId::FocusUp => &self.focus_up,
            ShortcutId::FocusDown => &self.focus_down,
            ShortcutId::CloseTab => &self.close_tab,
            ShortcutId::ToggleZoom => &self.toggle_zoom,
        }
    }

    pub fn set(&mut self, id: ShortcutId, combo: String) {
        let field = match id {
            ShortcutId::ToggleSidebar => &mut self.toggle_sidebar,
            ShortcutId::ToggleChanges => &mut self.toggle_changes,
            ShortcutId::ToggleTerminal => &mut self.toggle_terminal,
            ShortcutId::NewSession => &mut self.new_session,
            ShortcutId::NextSession => &mut self.next_session,
            ShortcutId::PrevSession => &mut self.prev_session,
            ShortcutId::SplitRight => &mut self.split_right,
            ShortcutId::SplitDown => &mut self.split_down,
            ShortcutId::FocusLeft => &mut self.focus_left,
            ShortcutId::FocusRight => &mut self.focus_right,
            ShortcutId::FocusUp => &mut self.focus_up,
            ShortcutId::FocusDown => &mut self.focus_down,
            ShortcutId::CloseTab => &mut self.close_tab,
            ShortcutId::ToggleZoom => &mut self.toggle_zoom,
        };
        *field = combo;
    }

    pub fn reset(&mut self, id: ShortcutId) {
        self.set(id, id.default_combo().to_string());
    }
}

/// Build a combo string from a recorded keystroke. The primary modifier
/// (cmd on macOS, ctrl elsewhere — either recorded key maps in) becomes "mod";
/// bare modifier presses record nothing.
pub fn combo_from_keystroke(
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    key: &str,
) -> Option<String> {
    combo_from_keystroke_on(cfg!(target_os = "macos"), ctrl, alt, shift, cmd, key)
}

/// [`combo_from_keystroke`] for an explicit platform — the ctrl spelling is
/// platform-dependent, so both paths need to be exercisable from one machine.
pub fn combo_from_keystroke_on(
    mac: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    key: &str,
) -> Option<String> {
    let key = key.trim().to_lowercase();
    if key.is_empty()
        || matches!(
            key.as_str(),
            "ctrl" | "control" | "alt" | "shift" | "cmd" | "platform" | "fn"
        )
    {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    // On macOS ctrl stays its own modifier rather than folding into "mod":
    // re-recording Ctrl+Tab as Cmd+Tab would hand the combo to the OS app
    // switcher, which never delivers it to the window.
    let ctrl_is_primary = ctrl && !mac;
    if cmd || ctrl_is_primary {
        parts.push("mod");
    }
    if ctrl && !ctrl_is_primary {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    parts.push(&key);
    Some(parts.join("-"))
}

/// Shortcut ids whose combos collide with another shortcut (conflict detection).
pub fn conflicted_shortcuts(keymap: &KeymapConfig) -> Vec<ShortcutId> {
    ShortcutId::ALL
        .into_iter()
        .filter(|&id| {
            let combo = keymap.get(id);
            !combo.is_empty()
                && ShortcutId::ALL
                    .into_iter()
                    .any(|other| other != id && keymap.get(other) == combo)
        })
        .collect()
}

/// Translate a stored combo into a bindable keystroke for this platform.
pub fn platform_combo(combo: &str) -> String {
    platform_combo_on(cfg!(target_os = "macos"), combo)
}

/// [`platform_combo`] for an explicit platform (see [`combo_from_keystroke_on`]).
pub fn platform_combo_on(mac: bool, combo: &str) -> String {
    let primary = if mac { "cmd" } else { "ctrl" };
    combo
        .split('-')
        .map(|part| if part == "mod" { primary } else { part })
        .collect::<Vec<_>>()
        .join("-")
}

/// Human-readable combo for the shortcuts table ("mod-s" → "Cmd+S"/"Ctrl+S").
pub fn display_combo(combo: &str) -> String {
    combo
        .split('-')
        .map(|part| match part {
            "mod" => {
                if cfg!(target_os = "macos") {
                    "Cmd".to_string()
                } else {
                    "Ctrl".to_string()
                }
            }
            "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

impl UiSettings {
    /// Clamp widths into their legal ranges (also heals NaN to defaults).
    pub fn clamped(mut self) -> Self {
        self.sidebar_width = clamp_or(
            self.sidebar_width,
            SIDEBAR_MIN,
            SIDEBAR_MAX,
            SIDEBAR_DEFAULT,
        );
        self.right_pane_width = clamp_or(
            self.right_pane_width,
            RIGHT_PANE_MIN,
            RIGHT_PANE_MAX,
            RIGHT_PANE_DEFAULT,
        );
        self.terminal_height = clamp_or(
            self.terminal_height,
            TERMINAL_MIN_HEIGHT,
            TERMINAL_ABS_MAX_HEIGHT,
            TERMINAL_DEFAULT_HEIGHT,
        );
        for dock in self.session_docks.values_mut() {
            *dock = dock.clamped();
        }
        self.cap_session_docks();
        self
    }

    /// Record `chat_id`'s docks (stamped `now_ms`), keeping at most
    /// [`SESSION_DOCKS_CAP`] entries.
    pub fn remember_session_dock(&mut self, chat_id: &str, mut dock: SessionDock, now_ms: i64) {
        dock.used_at = now_ms;
        self.session_docks.insert(chat_id.to_string(), dock);
        self.cap_session_docks();
    }

    /// The most recently changed session's docks.
    pub fn latest_session_dock(&self) -> Option<&SessionDock> {
        self.session_docks.values().max_by_key(|dock| dock.used_at)
    }

    /// Drop entries of sessions for which `keep` is false (deleted chats).
    pub fn prune_session_docks(&mut self, keep: impl Fn(&str) -> bool) {
        self.session_docks.retain(|chat_id, _| keep(chat_id));
    }

    fn cap_session_docks(&mut self) {
        let excess = self.session_docks.len().saturating_sub(SESSION_DOCKS_CAP);
        if excess == 0 {
            return;
        }
        let mut by_age: Vec<(i64, String)> = self
            .session_docks
            .iter()
            .map(|(id, dock)| (dock.used_at, id.clone()))
            .collect();
        by_age.sort();
        for (_, id) in by_age.into_iter().take(excess) {
            self.session_docks.remove(&id);
        }
    }

    /// Load from `{data_dir}/ui-settings.json`; defaults on any failure.
    /// Commands shown by default and not offered yet are turned on.
    pub fn load(data_dir: &Path) -> Self {
        let mut settings = match std::fs::read_to_string(Self::path(data_dir)) {
            Ok(text) => match serde_json::from_str::<UiSettings>(&text) {
                Ok(settings) => settings.clamped(),
                Err(err) => {
                    tracing::warn!(error = %err, "ui-settings corrupt; using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        };
        commands::offer_defaults(
            &mut settings.shown_slash_commands,
            &mut settings.offered_slash_commands,
        );
        settings
    }

    /// Write atomically (temp file + rename) so a crash mid-write never corrupts.
    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = Self::path(data_dir);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(FILE_NAME)
    }
}

/// Deserialize a field through [`serde_json::Value`], falling back to its
/// default (with a warning) when the value doesn't fit the type.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            field = std::any::type_name::<T>(),
            "ui-settings: dropping an unreadable entry"
        );
        T::default()
    }))
}

/// [`lenient`] per entry of a string-keyed map: an unreadable entry (or a
/// non-object map) is dropped, the rest survive.
fn lenient_map<'de, D, T>(deserializer: D) -> Result<HashMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Object(entries) = value else {
        tracing::warn!(
            field = std::any::type_name::<T>(),
            "ui-settings: dropping an unreadable map"
        );
        return Ok(HashMap::new());
    };
    Ok(entries
        .into_iter()
        .filter_map(|(key, value)| match serde_json::from_value(value) {
            Ok(entry) => Some((key, entry)),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    key = %key,
                    field = std::any::type_name::<T>(),
                    "ui-settings: dropping an unreadable entry"
                );
                None
            }
        })
        .collect())
}

fn clamp_or(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let settings = UiSettings {
            sidebar_width: 300.0,
            sidebar_collapsed: true,
            sidebar_grouped: true,
            last_space_id: Some("space-1".into()),
            open_tabs: Some(vec!["b".to_string(), "a".to_string()]),
            legacy_space_filter: Some("space-1".into()),
            tab_order: std::collections::HashMap::from([(
                "space-1".to_string(),
                vec!["b".to_string(), "a".to_string()],
            )]),
            space_order: vec!["space-2".to_string(), "space-1".to_string()],
            sound_enabled: false,
            notifications_enabled: false,
            notifications_background_only: false,
            dock_badge_enabled: false,
            right_pane_width: 700.0,
            right_pane_open: true,
            terminal_height: 320.0,
            terminal_open: true,
            keymap: KeymapConfig {
                toggle_sidebar: "mod-shift-s".into(),
                ..KeymapConfig::default()
            },
            appearance: crate::appearance::AppearanceMode::Light,
            setup_completed: true,
            pi_runtime_setup_version: 1,
            shown_slash_commands: vec!["goal".into(), "scripts".into()],
            offered_slash_commands: vec!["scripts".into()],
            sidebar_sort: SidebarSort::Device,
            sidebar_device_filter: Some("dev-1".into()),
            sidebar_sort_reversed: true,
            workspace: Some(sample_workspace()),
            project_workspaces: HashMap::from([("space-1".to_string(), sample_workspace())]),
            session_docks: HashMap::from([(
                "a".to_string(),
                SessionDock {
                    right: Some(0.4),
                    terminal: None,
                    right_open: true,
                    terminal_open: false,
                    used_at: 7,
                },
            )]),
        };
        settings.save(dir.path()).unwrap();
        assert_eq!(UiSettings::load(dir.path()), settings);
        let json = std::fs::read_to_string(UiSettings::path(dir.path())).unwrap();
        for key in [
            "\"workspace\"",
            "\"projectWorkspaces\"",
            "\"sessionDocks\"",
            "\"rightOpen\"",
        ] {
            assert!(json.contains(key), "{key} missing: {json}");
        }
    }

    fn sample_workspace() -> crate::workspace::Workspace {
        use crate::workspace::{Edge, TabKey, Workspace};
        let mut ws = Workspace::new();
        let first = ws.open(TabKey::session("a"));
        ws.open_split(TabKey::session("b"), first, Edge::Right);
        ws
    }

    /// Files from before the workspace layout load with no saved layout and
    /// no per-session docks — and don't write the keys back until used.
    #[test]
    fn legacy_files_without_a_workspace_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "openTabs": ["a"], "rightPaneWidth": 700, "terminalHeight": 300}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.workspace, None);
        assert!(loaded.project_workspaces.is_empty() && loaded.session_docks.is_empty());
        assert_eq!(loaded.right_pane_width, 700.0, "legacy sizes still seed");
        assert_eq!(loaded.terminal_height, 300.0);
        let json = serde_json::to_string(&loaded).unwrap();
        assert!(
            !json.contains("workspace") && !json.contains("sessionDocks"),
            "{json}"
        );
    }

    /// The old hidden-command list loads without error and leaves every
    /// command but the defaults hidden; it is not written back.
    #[test]
    fn legacy_hidden_slash_commands_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "hiddenSlashCommands": ["mcp", "skill:x"]}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.sidebar_width, 300.0, "the rest of the file loads");
        assert_eq!(loaded.shown_slash_commands, ["scripts"]);
        let json = serde_json::to_string(&loaded).unwrap();
        assert!(!json.contains("hiddenSlashCommands"), "{json}");
    }

    /// A layout of the wrong shape (a newer build's tab kind after a
    /// downgrade, a negative index) drops only itself: every other setting
    /// survives the load.
    #[test]
    fn a_type_invalid_workspace_drops_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{
                "keymap": {"toggleSidebar": "mod-shift-s"},
                "appearance": "light",
                "workspace": {"root": {"group": 1}, "groups": {"1": {"tabs": [{"futureKind": 3}], "active": -1}}},
                "projectWorkspaces": {"p": {"root": {"group": 1}, "groups": {"1": {"active": -2}}}, "q": {}},
                "sessionDocks": {"a": {"right": "wide"}, "b": {"rightOpen": true, "usedAt": 5}}
            }"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.keymap.toggle_sidebar, "mod-shift-s");
        assert_eq!(loaded.appearance, crate::appearance::AppearanceMode::Light);
        assert_eq!(loaded.workspace, None);
        assert_eq!(
            loaded.project_workspaces.keys().collect::<Vec<_>>(),
            vec!["q"]
        );
        assert_eq!(loaded.session_docks.keys().collect::<Vec<_>>(), vec!["b"]);
        assert!(loaded.session_docks["b"].right_open);
        // A map of the wrong type altogether is dropped the same way.
        let loaded: UiSettings =
            serde_json::from_str(r#"{"soundEnabled": false, "sessionDocks": [1, 2]}"#).unwrap();
        assert!(!loaded.sound_enabled);
        assert!(loaded.session_docks.is_empty());
    }

    #[test]
    fn a_corrupt_saved_workspace_is_repaired_not_fatal() {
        let loaded: UiSettings = serde_json::from_str(
            r#"{"soundEnabled": false, "workspace": {"root": {"group": 9}, "groups": {}}}"#,
        )
        .unwrap();
        assert!(!loaded.sound_enabled);
        let ws = loaded.workspace.expect("repaired, not dropped");
        assert_eq!(ws.group_count(), 1);
        assert_eq!(ws.tabs().count(), 0);
    }

    #[test]
    fn session_docks_heal_and_stay_bounded() {
        let mut settings = UiSettings::default();
        for i in 0..SESSION_DOCKS_CAP + 5 {
            settings.remember_session_dock(&format!("c{i}"), SessionDock::default(), i as i64);
        }
        assert_eq!(settings.session_docks.len(), SESSION_DOCKS_CAP);
        // The oldest went first; the newest is the default for new sessions.
        assert!(!settings.session_docks.contains_key("c0"));
        assert!(!settings.session_docks.contains_key("c4"));
        assert!(settings.session_docks.contains_key("c5"));
        let latest = settings.latest_session_dock().unwrap();
        assert_eq!(latest.used_at, (SESSION_DOCKS_CAP + 4) as i64);
        settings.prune_session_docks(|id| id == "c10");
        assert_eq!(settings.session_docks.len(), 1);

        let healed: UiSettings = serde_json::from_str(
            r#"{"sessionDocks": {"a": {"right": 3.0, "terminal": -1.0, "terminalOpen": true}}}"#,
        )
        .unwrap();
        let a = healed.clamped().session_docks["a"];
        assert_eq!(a.right, Some(DOCK_FRACTION_MAX));
        assert_eq!(a.terminal, Some(DOCK_FRACTION_MIN));
        assert!(a.terminal_open && !a.right_open);
        assert_eq!(clamp_fraction(f32::NAN), None);
    }

    #[test]
    fn a_new_session_starts_from_the_latest_sizes_with_docks_closed() {
        let latest = SessionDock {
            right: Some(0.3),
            terminal: Some(0.25),
            right_open: true,
            terminal_open: true,
            used_at: 5,
        };
        let seeded = SessionDock::seeded_from(Some(&latest));
        assert_eq!((seeded.right, seeded.terminal), (Some(0.3), Some(0.25)));
        assert!(!seeded.right_open && !seeded.terminal_open);
        assert_eq!(SessionDock::seeded_from(None), SessionDock::default());
    }

    /// The retired sidebar filter keeps its old `spaceFilter` JSON key — an
    /// explicitly legacy/unused compatibility field. Old files load it into
    /// `legacy_space_filter`; runtime behavior never reads it.
    #[test]
    fn space_filter_serializes_under_its_legacy_key() {
        let settings = UiSettings {
            legacy_space_filter: Some("space-1".into()),
            ..UiSettings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"spaceFilter\":\"space-1\""), "{json}");
        assert!(!json.contains("legacySpaceFilter"), "{json}");
        // And a pre-redesign file (with `spaceFilter`) still parses.
        let loaded: UiSettings =
            serde_json::from_str(r#"{"sidebarWidth": 280, "spaceFilter": "space-9"}"#).unwrap();
        assert_eq!(loaded.legacy_space_filter.as_deref(), Some("space-9"));
    }

    /// A settings file written before light mode existed has no `appearance`
    /// key; it must load as "follow the OS" rather than failing the whole parse
    /// and resetting every other preference to defaults.
    #[test]
    fn settings_without_appearance_default_to_system() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "soundEnabled": false}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.appearance, crate::appearance::AppearanceMode::System);
        assert_eq!(loaded.sidebar_width, 300.0);
        assert!(!loaded.sound_enabled, "other keys still parse");
        assert!(
            loaded.notifications_enabled,
            "pre-banner files default banners on"
        );
        assert!(
            loaded.notifications_background_only,
            "pre-banner files default background-only on"
        );
        assert!(
            loaded.dock_badge_enabled,
            "pre-badge files default the Dock badge on"
        );
        assert!(
            !loaded.setup_completed,
            "pre-setup files still show first-run setup"
        );
        let pre_runtime: UiSettings = serde_json::from_str(r#"{"setupCompleted":true}"#).unwrap();
        assert_eq!(
            pre_runtime.pi_runtime_setup_version, 0,
            "users who completed the old system-Pi setup see the runtime prompt once"
        );
    }

    #[test]
    fn missing_and_corrupt_files_yield_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(UiSettings::load(dir.path()), UiSettings::default());
        std::fs::write(UiSettings::path(dir.path()), "{not json").unwrap();
        assert_eq!(UiSettings::load(dir.path()), UiSettings::default());
    }

    #[test]
    fn loaded_values_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 10000, "rightPaneWidth": 1}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.sidebar_width, SIDEBAR_MAX);
        assert_eq!(loaded.right_pane_width, RIGHT_PANE_MIN);
    }

    #[test]
    fn nan_heals_to_default() {
        let healed = UiSettings {
            sidebar_width: f32::NAN,
            ..Default::default()
        }
        .clamped();
        assert_eq!(healed.sidebar_width, SIDEBAR_DEFAULT);
    }

    #[test]
    fn defaults_match_zeron() {
        let d = UiSettings::default();
        assert_eq!(d.sidebar_width, 256.0);
        assert_eq!(d.right_pane_width, 520.0);
        assert_eq!(d.terminal_height, 280.0);
        assert!(!d.sidebar_collapsed && !d.right_pane_open && !d.terminal_open);
    }

    #[test]
    fn keymap_defaults_and_reset() {
        let mut keymap = KeymapConfig::default();
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-s");
        assert_eq!(keymap.get(ShortcutId::ToggleChanges), "mod-b");
        assert_eq!(keymap.get(ShortcutId::ToggleTerminal), "mod-j");
        let ctrl = if cfg!(target_os = "macos") {
            "ctrl"
        } else {
            "mod"
        };
        assert_eq!(keymap.get(ShortcutId::NextSession), format!("{ctrl}-tab"));
        assert_eq!(
            keymap.get(ShortcutId::PrevSession),
            format!("{ctrl}-shift-tab")
        );
        keymap.set(ShortcutId::ToggleSidebar, "mod-shift-x".into());
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-shift-x");
        keymap.reset(ShortcutId::ToggleSidebar);
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-s");
    }

    #[test]
    fn combo_recording() {
        // How this platform spells a recorded ctrl (see `combo_from_keystroke_on`).
        let ctrl_combo = |suffix: &str| {
            if cfg!(target_os = "macos") {
                format!("ctrl-{suffix}")
            } else {
                format!("mod-{suffix}")
            }
        };
        assert_eq!(
            combo_from_keystroke(true, false, false, false, "s"),
            Some(ctrl_combo("s"))
        );
        assert_eq!(
            combo_from_keystroke(false, false, false, true, "s"),
            Some("mod-s".into())
        );
        assert_eq!(
            combo_from_keystroke(true, false, true, false, "tab"),
            Some(ctrl_combo("shift-tab"))
        );
        assert_eq!(
            combo_from_keystroke(true, true, true, false, "K"),
            Some(ctrl_combo("alt-shift-k"))
        );
        // Plain keys record without modifiers (Esc is filtered by the caller).
        assert_eq!(
            combo_from_keystroke(false, false, false, false, "f5"),
            Some("f5".into())
        );
        // Bare modifier presses record nothing.
        assert_eq!(
            combo_from_keystroke(true, false, false, false, "ctrl"),
            None
        );
        assert_eq!(
            combo_from_keystroke(false, false, true, false, "shift"),
            None
        );
        assert_eq!(combo_from_keystroke(false, false, false, false, ""), None);
    }

    #[test]
    fn every_default_is_spelled_the_way_the_recorder_spells_it() {
        // The invariant `default_combo_on` documents. Checked for BOTH
        // platforms because the hazard only exists off macOS, so a single-OS
        // CI run would never see it.
        for mac in [true, false] {
            for id in ShortcutId::ALL {
                let combo = id.default_combo_on(mac);
                // Via the platform spelling, where modifier names are
                // unambiguous, so the decode can't inherit the bug it checks.
                let bound = platform_combo_on(mac, combo);
                let mut parts: Vec<&str> = bound.split('-').collect();
                let key = parts.pop().expect("a combo always ends in a key");
                let recorded = combo_from_keystroke_on(
                    mac,
                    parts.contains(&"ctrl"),
                    parts.contains(&"alt"),
                    parts.contains(&"shift"),
                    parts.contains(&"cmd"),
                    key,
                );
                assert_eq!(
                    recorded.as_deref(),
                    Some(combo),
                    "{} default {combo:?} is unreachable from the recorder (mac={mac})",
                    id.label()
                );
            }
        }
    }

    #[test]
    fn defaults_are_distinct_physical_keys() {
        // Distinct STRINGS is not enough — two defaults could still resolve to
        // the same keystroke through `platform_combo`.
        let mut seen = std::collections::HashSet::new();
        for id in ShortcutId::ALL {
            let bound = platform_combo(id.default_combo());
            assert!(seen.insert(bound.clone()), "{bound:?} bound twice");
        }
    }

    #[test]
    fn conflict_detection() {
        let mut keymap = KeymapConfig::default();
        assert!(conflicted_shortcuts(&keymap).is_empty());
        keymap.set(ShortcutId::ToggleChanges, "mod-s".into());
        let conflicts = conflicted_shortcuts(&keymap);
        assert!(conflicts.contains(&ShortcutId::ToggleSidebar));
        assert!(conflicts.contains(&ShortcutId::ToggleChanges));
        assert!(!conflicts.contains(&ShortcutId::ToggleTerminal));
        keymap.reset(ShortcutId::ToggleChanges);
        assert!(conflicted_shortcuts(&keymap).is_empty());
    }

    #[test]
    fn combo_translation() {
        let primary = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        assert_eq!(platform_combo("mod-s"), format!("{primary}-s"));
        assert_eq!(platform_combo("alt-f4"), "alt-f4");
        let display_primary = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        assert_eq!(
            display_combo("mod-shift-s"),
            format!("{display_primary}+Shift+S")
        );
        assert_eq!(display_combo("f5"), "F5");
        // Literal ctrl passes through untouched — the macOS spelling of
        // session cycling.
        assert_eq!(platform_combo("ctrl-shift-tab"), "ctrl-shift-tab");
        assert_eq!(display_combo("ctrl-shift-tab"), "Ctrl+Shift+Tab");
    }

    #[test]
    fn keymap_survives_old_settings_files() {
        // Files written before the keymap existed load with defaults.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(UiSettings::path(dir.path()), r#"{"sidebarWidth": 300}"#).unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.keymap, KeymapConfig::default());
        assert!(!loaded.sidebar_grouped);
    }

    #[test]
    fn a_keymap_missing_newer_shortcuts_keeps_its_customizations() {
        // Upgrade path: a file from a build that predates session cycling
        // carries the user's rebinds and defaults only the new rows.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"keymap": {"toggleSidebar": "mod-shift-x"}}"#,
        )
        .unwrap();
        let keymap = UiSettings::load(dir.path()).keymap;
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-shift-x");
        assert_eq!(keymap.get(ShortcutId::ToggleTerminal), "mod-j");
        assert_eq!(
            keymap.get(ShortcutId::NextSession),
            ShortcutId::NextSession.default_combo()
        );
        // The workspace rows arrived later still: defaults, customizations
        // intact.
        assert_eq!(keymap.get(ShortcutId::SplitRight), "mod-\\");
        assert_eq!(keymap.get(ShortcutId::ToggleZoom), "mod-shift-enter");
    }

    #[test]
    fn workspace_shortcuts_are_grouped_and_round_trip() {
        let workspace: Vec<ShortcutId> = ShortcutId::ALL
            .into_iter()
            .filter(|id| id.group() == ShortcutGroup::Workspace)
            .collect();
        assert_eq!(workspace.len(), 8);
        assert_eq!(ShortcutId::NewSession.group(), ShortcutGroup::General);
        let mut keymap = KeymapConfig::default();
        keymap.set(ShortcutId::FocusLeft, "mod-shift-left".into());
        let json = serde_json::to_string(&keymap).unwrap();
        assert!(json.contains(r#""focusLeft":"mod-shift-left""#), "{json}");
        let back: KeymapConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, keymap);
    }

    #[test]
    fn terminal_height_clamps_on_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(UiSettings::path(dir.path()), r#"{"terminalHeight": 5}"#).unwrap();
        assert_eq!(
            UiSettings::load(dir.path()).terminal_height,
            TERMINAL_MIN_HEIGHT
        );
        std::fs::write(UiSettings::path(dir.path()), r#"{"terminalHeight": 99999}"#).unwrap();
        assert_eq!(
            UiSettings::load(dir.path()).terminal_height,
            TERMINAL_ABS_MAX_HEIGHT
        );
    }
}
