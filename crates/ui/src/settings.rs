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

/// Terminal panel height bounds: 160px … 55% of the viewport. The
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
    /// The last selected space — restored on boot when the row still exists;
    /// also the new-session canvas's default target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_space_id: Option<String>,
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
    /// Legacy global terminal height (px) — see [`Self::right_pane_width`].
    pub terminal_height: f32,
    /// Customizable shortcut combos.
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
    /// The main window's workspace layout (docs/design/workspace-layout.md,
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
            last_space_id: None,
            sound_enabled: true,
            notifications_enabled: true,
            notifications_background_only: true,
            dock_badge_enabled: true,
            right_pane_width: RIGHT_PANE_DEFAULT,
            terminal_height: TERMINAL_DEFAULT_HEIGHT,
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
// Keymap (customizable shortcuts)
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
    // Workspace layout (docs/design/workspace-layout.md).
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
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        crate::fs_util::write_atomic(data_dir, FILE_NAME, json.as_bytes(), 0o666)
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
mod tests;
