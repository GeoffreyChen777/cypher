//! App state: the engine connection, entity lists, and the selected chat's
//! transcript — one gpui [`Entity`] the whole shell renders from.
//!
//! ## EngineHandle
//! The UI talks the same typed RPC whether the engine is in-process or a separate
//! daemon (ARCHITECTURE §1). [`EngineHandle::bootstrap`] probes the localhost IPC
//! port, mirroring zeron: if an engine is listening it connects over WebSocket
//! ([`RemoteEngine`]); otherwise it embeds one via [`EngineCore::assemble`] and an
//! in-memory RPC transport ([`InProcessEngine`]) — same envelopes, same dispatch.
//!
//! ## Async bridging
//! `bootstrap` runs on tokio via `gpui_tokio::Tokio::spawn`. Once an [`RpcClient`]
//! exists, its `call`/`subscribe` futures are runtime-agnostic (tokio channels),
//! so subscription pumps run on gpui's own executor via `cx.spawn` and fold each
//! frame into the entity with `this.update(...)` + `cx.notify()`.
//!
//! Pure logic (sort order, staleness, gate phase) lives in free functions with
//! unit tests; rendering reads them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gpui::{App, AppContext, Context, Entity, Subscription, Task, WeakEntity};
use gpui_tokio::Tokio;
use serde::de::DeserializeOwned;

use cypher_doc::{
    SessionCommandEntry, SessionCommandPayload, SessionCommandStatus, SessionMessageEntry,
    TranscriptDesync, TranscriptFrame,
};
use cypher_proto::{
    AuthState, Chat, ChatIndicator, Device, Session, SideChatStatus, Space, WorkspaceScope,
};
use cypher_rpc::methods;

use crate::prefs::SidebarSort;
mod engine;
pub use engine::*;
mod glue;
use glue::*;
mod queries;
mod reducers;

// ---------------------------------------------------------------------------
// Pure state + reducers
// ---------------------------------------------------------------------------

// The frontend-agnostic derivations (sort orders, staleness gating, sidebar
// grouping, the boot gate, relative times) live in `cypher_proto::view`, pure
// and with their own test suite. Re-exported here because every call site in
// this crate reads them as `state::…`.
pub use cypher_proto::view::{
    ConnectionStatus, GatePhase, Indicator, chat_location, display_status, effective_indicator,
    format_time_ago, gate_phase, parse_auth_state, sort_active, sort_chats, sort_spaces, sort_tabs,
};

/// A device that pinged within this window shows a presence dot (engines
/// heartbeat every 15s; 70s tolerates a couple of missed beats).
pub const DEVICE_ONLINE_WINDOW_SECS: i64 = 70;

/// Presence: last-seen within the online window (future timestamps count). Pure.
pub fn device_online(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_seen
        .is_some_and(|at| now.signed_duration_since(at).num_seconds() <= DEVICE_ONLINE_WINDOW_SECS)
}

// ---------------------------------------------------------------------------
// Org gate (pure)
// ---------------------------------------------------------------------------

/// One org membership row (tolerant local mirror of the engine's ListOrgs
/// reply — `{orgs: [{id, organizationId, name}]}`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRow {
    pub organization_id: String,
    pub name: String,
}

/// Parse a ListOrgs reply tolerantly (accepts a bare array too).
pub fn parse_orgs(value: &serde_json::Value) -> Vec<OrgRow> {
    let list = value.get("orgs").unwrap_or(value);
    serde_json::from_value(list.clone()).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrgSetup {
    AutoCreate,
    AutoSelect(String),
    Pick(Vec<OrgRow>),
}

/// Decide organization setup after normalizing the membership list.
pub fn org_setup(rows: Vec<OrgRow>) -> OrgSetup {
    let rows = sort_memberships(rows);
    match rows.as_slice() {
        [] => OrgSetup::AutoCreate,
        [only] => OrgSetup::AutoSelect(only.organization_id.clone()),
        _ => OrgSetup::Pick(rows),
    }
}

/// Memberships sorted by name (case-insensitive), deduped by organization id.
pub fn sort_memberships(mut orgs: Vec<OrgRow>) -> Vec<OrgRow> {
    orgs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    orgs.dedup_by(|a, b| a.organization_id == b.organization_id);
    orgs
}

// ---------------------------------------------------------------------------
// AppState entity
// ---------------------------------------------------------------------------

/// A composer send whose doc command is queued but not yet executed by the
/// chat's host device — cleared when the host writes the user message back
/// into the transcript (same client-minted id as the [`AppState::echoes`]
/// dedup), or after [`PENDING_SEND_TTL_MS`].
#[derive(Debug, Clone)]
struct PendingSend {
    message_id: String,
    started: DateTime<Utc>,
}

struct UploadProgress {
    /// The chat whose send owns this upload. The trailer is scoped to it so a
    /// background upload never narrates itself under someone else's
    /// conversation.
    chat_id: String,
    total_bytes: u64,
    completed_bytes: Arc<std::sync::atomic::AtomicU64>,
}

/// How long the send-in-flight overlay may hold before the synced status
/// shows through again. Covers the queue → nudge → drain → sync round-trip
/// to a remote host; when the host is offline the dot falls back to the
/// truth after this.
pub const PENDING_SEND_TTL_MS: i64 = 30_000;

/// Projected status of one queued message command, mapped from the durable
/// ledger by `message_id` (Run/Steer commands only). This is the source of
/// truth over the local optimistic overlay: the composer's send-in-flight
/// state guesses, the doc ledger knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSendStatus {
    /// A live pending attempt exists and no earlier attempt failed.
    Queued,
    /// A live pending attempt exists AFTER an earlier failure — a retry is
    /// in flight.
    Retrying,
    /// The latest attempt failed (Rejected/Expired) and awaits a retry.
    Failed,
}

/// The logical message id a Run/Steer command carries (the same client-minted
/// id as the optimistic echo — the dedup key across attempts).
pub fn command_message_id(payload: &SessionCommandPayload) -> Option<&str> {
    match payload {
        SessionCommandPayload::Run { message_id, .. } => Some(message_id),
        SessionCommandPayload::Steer {
            message_id: Some(id),
            ..
        } => Some(id),
        _ => None,
    }
}

/// Map a message id to its projected send status from the durable ledger.
/// `None` = no Run/Steer command (or the message already resolved).
pub fn command_send_status(
    commands: &[SessionCommandEntry],
    message_id: &str,
) -> Option<CommandSendStatus> {
    let attempts: Vec<&SessionCommandEntry> = commands
        .iter()
        .filter(|c| command_message_id(&c.payload) == Some(message_id))
        .collect();
    if attempts.is_empty() {
        return None;
    }
    let has_live = attempts
        .iter()
        .any(|c| c.status == SessionCommandStatus::Pending);
    let has_failed = attempts.iter().any(|c| {
        matches!(
            c.status,
            SessionCommandStatus::Rejected | SessionCommandStatus::Expired
        )
    });
    if has_live {
        if has_failed {
            Some(CommandSendStatus::Retrying)
        } else {
            Some(CommandSendStatus::Queued)
        }
    } else if has_failed {
        Some(CommandSendStatus::Failed)
    } else {
        None
    }
}

/// A failed (Rejected/Expired) message command the user can retry — the
/// composer's failed row. One per message id, always the LATEST attempt.
#[derive(Debug, Clone)]
pub struct FailedCommand {
    pub command_id: String,
    pub prompt: String,
    pub resolution: Option<String>,
}

/// The retry-able failures in the ledger, in doc order, skipping messages
/// with a live pending attempt (their retry is already in flight).
pub fn failed_commands(commands: &[SessionCommandEntry]) -> Vec<FailedCommand> {
    let mut out: Vec<FailedCommand> = Vec::new();
    for command in commands {
        let Some(message_id) = command_message_id(&command.payload) else {
            continue;
        };
        // A live attempt supersedes the failed row: Retrying is in flight.
        if command_send_status(commands, message_id) != Some(CommandSendStatus::Failed) {
            continue;
        }
        // Latest failed attempt only: a retry that failed again keeps the
        // newest command id as the retry target.
        let is_latest = commands.iter().any(|other| {
            other.id != command.id
                && command_message_id(&other.payload) == Some(message_id)
                && other.issued_at > command.issued_at
        });
        if is_latest {
            continue;
        }
        let prompt = match &command.payload {
            SessionCommandPayload::Run { request, .. } => request.prompt.clone(),
            SessionCommandPayload::Steer { prompt, .. } => prompt.clone(),
            _ => continue,
        };
        out.push(FailedCommand {
            command_id: command.id.clone(),
            prompt,
            resolution: command.resolution.clone(),
        });
    }
    out
}

/// Root application state. Reducer methods (`apply_*`, [`Self::session_for`], …)
/// are plain `&mut self` functions so tests construct the struct directly; gpui
/// glue ([`Self::bootstrap`], [`Self::select_chat`]) layers subscriptions on top.
pub struct AppState {
    pub connection: ConnectionStatus,
    /// Fixed data boundary of the attached engine. Authentication may change
    /// in place, but changing this scope requires assembling a new runtime.
    pub workspace_scope: Option<WorkspaceScope>,
    /// Auth stream value; `None` until the engine reports one.
    pub auth: Option<AuthState>,
    pub devices: Vec<Device>,
    /// Sorted (see [`sort_spaces`]).
    pub spaces: Vec<Space>,
    /// Sorted (see [`sort_chats`]); includes archived rows — views filter.
    pub chats: Vec<Chat>,
    pub sessions: Vec<Session>,
    /// The project the new-session canvas mints into. Healed by
    /// [`Self::apply_spaces`] when the row vanishes; selecting a chat implies
    /// its project.
    pub selected_space: Option<String>,
    /// Deliberate "Don't work in a project" pick: while set, the canvas mints
    /// project-less sessions (cwd `~` on the picked device) and
    /// [`Self::selected_space_row`] reads as `None` — healing must NOT
    /// re-select a project underneath it.
    pub no_project: bool,
    /// The canvas is a QUICK CHAT: the next send asks the picked device for a
    /// throwaway scratch folder and runs there (implies `no_project`).
    /// Cleared by any project pick, chat selection, or the ordinary new
    /// session.
    pub scratch_pending: bool,
    /// The composer's device pick — where project-less sessions run, and the
    /// device whose projects the project picker lists. `None` falls back to
    /// the local device.
    pub selected_device: Option<String>,
    pub selected_chat: Option<String>,
    /// Boot auto-select happened (or a manual selection superseded it).
    pub auto_selected: bool,
    /// First chats / spaces watch frame has landed — device-local state that
    /// prunes against the doc (open tabs) must not judge by the empty
    /// pre-sync lists.
    pub chats_synced: bool,
    pub spaces_synced: bool,
    /// Bumped per applied chats frame. A session context mirrors it and
    /// heals a vanished selection only when it advances — like
    /// [`Self::apply_chats`], never on an unrelated notify (a canvas tile
    /// selects its minted chat before the row's frame lands).
    chats_generation: u64,
    /// Joined transcript of the selected chat (continuations folded engine-side).
    pub transcript: Vec<SessionMessageEntry>,
    /// Durable command ledger of the selected chat (WatchDocCommands): the
    /// source of truth the UI projects Queued/Failed/Retrying from.
    commands: Vec<SessionCommandEntry>,
    /// Optimistic user echoes per chat id, shown until the doc frame carrying
    /// the same message id arrives (client-minted ids make dedup exact).
    echoes: HashMap<String, Vec<SessionMessageEntry>>,
    /// Message ids this device sent as a Steer. Labels the optimistic echo
    /// before the ledger frame carrying its Steer command arrives.
    local_steers: HashSet<String>,
    /// Send-in-flight overlay per chat id: a queued doc command the host
    /// hasn't executed yet (see [`Self::begin_pending_send`]). Shared between
    /// a main state and its session contexts ([`Self::new_session_context`])
    /// so a send from any tile drives the main sidebar's dot and chime gate;
    /// a project window keeps its own copy.
    pending_sends: Rc<RefCell<HashMap<String, PendingSend>>>,
    upload_progress: Option<UploadProgress>,
    /// This engine's device id (from `EngineInfo` at attach; `None` until an
    /// engine is attached).
    pub local_device_id: Option<String>,
    /// Latest `UpdateStatus` frame — drives the sidebar update strip.
    pub update: Option<cypher_update::UpdateStatus>,
    /// Latest Pi CLI + extension update facts — drives the one-click package
    /// update notification beside the Cypher release strip.
    pub pi_update: Option<cypher_engine::pi::packages::PiUpdateStatus>,
    /// Data directory (`ui-settings.json`, `composer-defaults.json`); set at
    /// bootstrap so child views can persist small preference files.
    pub data_dir: Option<PathBuf>,
    engine: Option<EngineHandle>,
    watch_tasks: Vec<Task<()>>,
    transcript_task: Option<Task<()>>,
    commands_task: Option<Task<()>>,
    /// Which projects this state's window lists (see [`ProjectScope`]).
    scope: ProjectScope,
    /// Whether `select_chat` subscribes the selected chat's transcript and
    /// command ledger. Off on a main state whose session tiles own the
    /// transcripts ("lists-only": the selection only drives the sidebar).
    transcript_watches: bool,
    /// Session context: the main state this one mirrors lists from and
    /// forwards seen / config writes to ([`Self::new_session_context`]).
    parent: Option<WeakEntity<AppState>>,
    /// Session context: the observation copying `parent`'s lists on each
    /// notify. Dropped with the context.
    mirror: Option<Subscription>,
    /// Bumped whenever what a transcript view renders may have changed
    /// (transcript, echoes, command ledger, steers, selection) — views gate
    /// their row rebuild on it ([`Self::transcript_revision`]).
    transcript_rev: u64,
}

/// Which projects a window lists. The main window lists every project except
/// the ones open in their own window; a project window lists only its
/// project (no project-less sessions). Scoping is a view concern: the lists
/// ([`AppState::visible_chats`], [`AppState::spaces_sorted`], the sidebar
/// groups) narrow, the synced rows underneath stay complete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectScope {
    /// The project a project window is dedicated to.
    pub only: Option<String>,
    /// Main window: projects currently open in their own windows.
    pub hidden: HashSet<String>,
}

impl ProjectScope {
    pub fn space_visible(&self, space_id: &str) -> bool {
        match self.only.as_deref() {
            Some(only) => only == space_id,
            None => !self.hidden.contains(space_id),
        }
    }

    pub fn chat_visible(&self, chat: &Chat) -> bool {
        match chat.space_id.as_deref() {
            Some(space_id) => self.space_visible(space_id),
            None => self.only.is_none(),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// The sidebar view menu's state: device filter + card sort.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarView {
    pub device: Option<String>,
    pub sort: SidebarSort,
    /// Flip the sort's natural direction (see
    /// [`SidebarSort::natural_descending`]).
    pub reversed: bool,
}

impl SidebarView {
    /// Whether the view currently reads newest/Z first.
    pub fn descending(&self) -> bool {
        self.sort.natural_descending() != self.reversed
    }
}

/// What kind of sidebar card a group is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarGroupKind {
    /// A live `Space` — has a project context menu; empty spaces included.
    Space,
    /// Project-less (`space_id = None`) chats of one device.
    NoProject,
    /// Quick chats (project-less, scratch-folder cwd) of one device.
    Scratch,
    /// Chats whose `space_id` names a missing space.
    Unavailable,
}

/// One card of the project-grouped sidebar, produced by
/// [`AppState::sidebar_groups`]. Synthetic cards (No project / Unavailable
/// project) carry no `space_id` and therefore no project context menu.
#[derive(Debug)]
pub struct SidebarGroup<'a> {
    /// Stable key: `s:<space id>` live space, `np:<device id>` no-project,
    /// `sc` quick chats (one card across every device), `u:<missing space
    /// id>` unavailable. Status changes never re-key a group, so cards keep
    /// their identity across renders.
    pub key: String,
    pub kind: SidebarGroupKind,
    /// Card title: the project's display name, "No project", or
    /// "Unavailable project".
    pub title: String,
    /// Folder path for live spaces (muted truncated in the header); `None`
    /// for synthetic cards.
    pub path: Option<String>,
    /// Host device name. Empty on a Quick chats card merged from several
    /// devices (each session names its own host).
    pub device: String,
    /// Host device id (the sidebar device filter's key).
    pub device_id: String,
    /// Host offline (live spaces only; synthetic cards read as online).
    pub offline: bool,
    /// Creation instant for the Date sort: the space's `created_at`, or the
    /// newest chat's for synthetic cards.
    pub created_at: DateTime<Utc>,
    /// The space id for live-space cards (the project context menu target).
    pub space_id: Option<&'a str>,
    /// User pin on the project (live spaces only): pinned cards lead the list.
    pub pinned: bool,
    /// Sidebar glyph/colour keys (live spaces only; see `space_style`).
    pub icon: Option<String>,
    pub color: Option<String>,
    /// The card's chats in overview recency order, pinned sessions first
    /// (empty for quiet spaces).
    pub chats: Vec<(ChatIndicator, &'a Chat)>,
}

/// Fold every per-device Quick chats card into one `sc` card at the first
/// (newest) one's position: quick chats are throwaway, so one card per host
/// only repeated the same header. The merged sessions return to overview
/// order; a card drawn from several hosts drops its single device name.
fn merge_scratch_groups(groups: &mut Vec<SidebarGroup<'_>>) {
    let Some(first) = groups
        .iter()
        .position(|g| g.kind == SidebarGroupKind::Scratch)
    else {
        return;
    };
    let mut chats = Vec::new();
    let mut devices = HashSet::new();
    let mut ix = first + 1;
    while ix < groups.len() {
        if groups[ix].kind == SidebarGroupKind::Scratch {
            let group = groups.remove(ix);
            devices.insert(group.device_id);
            chats.extend(group.chats);
        } else {
            ix += 1;
        }
    }
    let card = &mut groups[first];
    card.key = "sc".into();
    if chats.is_empty() {
        return;
    }
    devices.remove(&card.device_id);
    if !devices.is_empty() {
        card.device.clear();
    }
    card.chats.extend(chats);
    sort_active(&mut card.chats);
    card.created_at = card
        .chats
        .iter()
        .map(|(_, c)| c.created_at)
        .max()
        .unwrap_or(card.created_at);
}

impl AppState {
    pub fn new() -> Self {
        Self {
            connection: ConnectionStatus::Connecting,
            workspace_scope: None,
            auth: None,
            devices: Vec::new(),
            spaces: Vec::new(),
            chats: Vec::new(),
            sessions: Vec::new(),
            selected_space: None,
            no_project: false,
            scratch_pending: false,
            selected_device: None,
            selected_chat: None,
            transcript: Vec::new(),
            commands: Vec::new(),
            echoes: HashMap::new(),
            local_steers: HashSet::new(),
            pending_sends: Rc::default(),
            upload_progress: None,
            local_device_id: None,
            update: None,
            pi_update: None,
            data_dir: None,
            engine: None,
            watch_tasks: Vec::new(),
            transcript_task: None,
            commands_task: None,
            auto_selected: false,
            chats_synced: false,
            spaces_synced: false,
            chats_generation: 0,
            scope: ProjectScope::default(),
            transcript_watches: true,
            parent: None,
            mirror: None,
            transcript_rev: 0,
        }
    }

    /// The state behind a project window: a secondary [`AppState`] sharing
    /// `main`'s [`EngineHandle`], scoped to `space_id`, with its own
    /// selection and transcript watches. Seeded from `main`'s synced lists so
    /// the window's first frame is already populated; its own watches take
    /// over from there. Lands on `main`'s selected chat when that belongs to
    /// the project (the chat moves windows with it), else on the project's
    /// most recent session. `None` while `main` has no engine attached.
    pub fn new_project_window(
        main: &Entity<AppState>,
        space_id: &str,
        cx: &mut App,
    ) -> Option<Entity<AppState>> {
        let m = main.read(cx);
        let engine = m.engine.clone()?;
        let space = m.space_row(space_id)?.clone();
        let in_project = |chat_id: &String| {
            m.chats
                .iter()
                .any(|c| &c.id == chat_id && c.space_id.as_deref() == Some(space_id))
        };
        let landing = m.selected_chat.clone().filter(|id| in_project(id));
        let mut seed = AppState::new();
        seed.scope.only = Some(space_id.to_string());
        seed.auth = m.auth.clone();
        seed.devices = m.devices.clone();
        seed.spaces = m.spaces.clone();
        seed.chats = m.chats.clone();
        seed.sessions = m.sessions.clone();
        seed.chats_synced = m.chats_synced;
        seed.spaces_synced = m.spaces_synced;
        seed.update = m.update.clone();
        seed.pi_update = m.pi_update.clone();
        seed.data_dir = m.data_dir.clone();
        // In-flight sends ride along so the moved sessions keep their
        // optimistic echoes and Working dots until the host acks.
        seed.echoes = m
            .echoes
            .iter()
            .filter(|(id, _)| in_project(id))
            .map(|(id, echoes)| (id.clone(), echoes.clone()))
            .collect();
        // A copy, not the shared map: the window runs its own watches.
        seed.pending_sends = Rc::new(RefCell::new(
            m.pending_sends
                .borrow()
                .iter()
                .filter(|(id, _)| in_project(id))
                .map(|(id, send)| (id.clone(), send.clone()))
                .collect(),
        ));
        seed.local_steers = m.local_steers.clone();
        seed.selected_space = Some(space.id.clone());
        seed.selected_device = Some(space.device_id.clone());
        let state = cx.new(|_| seed);
        state.update(cx, |s, cx| {
            s.attach_engine(engine, false, cx);
            let landing = landing.or_else(|| {
                s.overview_chats(Utc::now())
                    .first()
                    .map(|(_, c)| c.id.clone())
            });
            if landing.is_some() {
                s.select_chat(landing, cx);
            }
        });
        Some(state)
    }

    /// A session context: a secondary [`AppState`] pinned to one session so
    /// several sessions render side by side, each through the unchanged
    /// `Transcript` / `Composer` / `Changes` / `FilesPanel` /
    /// `TerminalPanel` (which read `selected_chat`, `transcript`, …).
    /// `chat_id: None` is a new-session canvas tile: it starts from `main`'s
    /// project / device picks, and its first send selects the new chat here.
    ///
    /// It runs no list watches of its own: it MIRRORS `main` (engine,
    /// connection, auth, devices, spaces, chats, sessions, …) on every
    /// notify, and only subscribes its own selected chat's transcript and
    /// ledger. The send-in-flight overlay is shared with `main`; seen marks
    /// and optimistic config writes are forwarded to it
    /// ([`Self::mark_chat_seen`], [`Self::set_chat_config_optimistic`]).
    pub fn new_session_context(
        main: &Entity<AppState>,
        chat_id: Option<String>,
        cx: &mut App,
    ) -> Entity<AppState> {
        let m = main.read(cx);
        let mut seed = AppState::new();
        seed.parent = Some(main.downgrade());
        seed.pending_sends = m.pending_sends.clone();
        seed.engine = m.engine.clone();
        seed.mirror_lists(m);
        seed.selected_space = m.selected_space.clone();
        seed.selected_device = m.selected_device.clone();
        seed.no_project = m.no_project;
        seed.scratch_pending = chat_id.is_none() && m.scratch_pending;
        seed.auto_selected = true;
        if let Some(id) = chat_id.as_deref()
            && let Some(echoes) = m.echoes.get(id)
        {
            seed.echoes.insert(id.to_string(), echoes.clone());
        }
        seed.local_steers = m.local_steers.clone();
        let state = cx.new(|cx| {
            seed.mirror = Some(cx.observe(main, |this: &mut AppState, main, cx| {
                this.mirror_from_parent(&main, cx);
            }));
            seed
        });
        if chat_id.is_some() {
            state.update(cx, |s, cx| s.select_chat(chat_id, cx));
        }
        state
    }

    /// The main state a session context mirrors (`None` elsewhere, or once
    /// the parent is gone).
    pub fn parent(&self) -> Option<Entity<AppState>> {
        self.parent.as_ref()?.upgrade()
    }

    /// Copy the synced lists from `main` (session contexts), then heal a
    /// selection that vanished the same way the watches' reducers do.
    /// Returns whether anything changed (unchanged lists aren't re-cloned).
    fn mirror_lists(&mut self, main: &AppState) -> bool {
        let mut changed = false;
        macro_rules! mirror {
            ($($field:ident),* $(,)?) => {$(
                if self.$field != main.$field {
                    self.$field = main.$field.clone();
                    changed = true;
                }
            )*};
        }
        mirror!(
            connection,
            workspace_scope,
            auth,
            devices,
            spaces,
            chats,
            sessions,
            local_device_id,
            data_dir,
            update,
            pi_update,
            chats_synced,
            spaces_synced,
            scope,
        );
        // Only a chats frame judges the selection (see `chats_generation`);
        // pre-sync (or mid runtime replacement) lists are empty, not
        // authoritative.
        let chats_frame = self.chats_generation != main.chats_generation;
        self.chats_generation = main.chats_generation;
        if self.chats_synced && chats_frame && self.drop_vanished_chat() {
            changed = true;
        }
        if self.spaces_synced {
            let before = (
                self.selected_space.clone(),
                self.selected_device.clone(),
                self.no_project,
            );
            self.heal_space_selection();
            changed |= before
                != (
                    self.selected_space.clone(),
                    self.selected_device.clone(),
                    self.no_project,
                );
        }
        changed
    }

    /// The mirror observation: lists, plus the engine — a runtime
    /// replacement on `main` re-aims (or drops) this context's watches.
    /// Notifies only when something was mirrored: every main notify reaches
    /// every tile's context, and a no-op notify re-rendered them all.
    fn mirror_from_parent(&mut self, main: &Entity<AppState>, cx: &mut Context<Self>) {
        let (mut changed, engine) = {
            let main = main.read(cx);
            (self.mirror_lists(main), main.engine.clone())
        };
        let same_engine = match (&self.engine, &engine) {
            (Some(a), Some(b)) => Arc::ptr_eq(&a.inner, &b.inner),
            (None, None) => true,
            _ => false,
        };
        if !same_engine {
            self.engine = engine;
            self.transcript.clear();
            self.commands.clear();
            self.transcript_task = None;
            self.commands_task = None;
            self.bump_transcript();
            self.spawn_transcript_watches(cx);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// A closed tab's context kept alive only by a parked terminal panel
    /// (its PTYs outlive the tab): stop mirroring `main` and drop the
    /// transcript watches — nothing renders from it until the panel is
    /// re-bound to a new tile's context.
    pub fn park_session_context(&mut self) {
        self.mirror = None;
        self.transcript_task = None;
        self.commands_task = None;
        self.transcript.clear();
        self.commands.clear();
        self.bump_transcript();
    }

    /// See [`Self::chats_generation`]: advances once per applied chats
    /// frame (never on an optimistic insert).
    pub fn chats_generation(&self) -> u64 {
        self.chats_generation
    }

    /// See [`Self::transcript_rev`].
    pub fn transcript_revision(&self) -> u64 {
        self.transcript_rev
    }

    fn bump_transcript(&mut self) {
        self.transcript_rev = self.transcript_rev.wrapping_add(1);
    }

    /// Replace the selected chat's transcript wholesale (a promoted side
    /// chat's handoff seeds the new tile before its doc watch lands).
    pub fn set_transcript(&mut self, entries: Vec<SessionMessageEntry>) {
        self.transcript = entries;
        self.bump_transcript();
    }

    pub fn project_scope(&self) -> &ProjectScope {
        &self.scope
    }

    /// The project a project window is dedicated to (`None` in the main
    /// window).
    pub fn window_project(&self) -> Option<&str> {
        self.scope.only.as_deref()
    }

    /// Main window: hide the projects that are open in their own windows.
    /// A selection that just left the scope moves on — the chat to the most
    /// recent listed session (else the canvas), the canvas project to the
    /// first listed one.
    pub fn set_hidden_projects(&mut self, hidden: HashSet<String>, cx: &mut Context<Self>) {
        if self.scope.hidden == hidden {
            return;
        }
        self.scope.hidden = hidden;
        if self
            .selected_chat_row()
            .is_some_and(|chat| !self.scope.chat_visible(chat))
        {
            let next = self
                .overview_chats(Utc::now())
                .first()
                .map(|(_, c)| c.id.clone());
            self.select_chat(next, cx);
        }
        if self
            .selected_space
            .as_deref()
            .is_some_and(|id| !self.scope.space_visible(id))
        {
            self.selected_space = self.first_space_on_picked_device();
        }
        cx.notify();
    }
}
#[cfg(test)]
mod tests;
