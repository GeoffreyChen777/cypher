//! Composer pickers: RepoPicker (recents + search +
//! in-app folder browser + clone/create), BranchPicker (search + isolated-
//! worktree toggle), HarnessModelPicker (provider rail + model list, Pi
//! locked once the chat exists), TraitsPicker (reasoning ladder + advertised
//! model options; trigger shows the non-default summary "High · 1M · Fast").
//!
//! All selections accumulate into a [`DraftConfig`] the composer threads into
//! the Run command and the `Mutate createChat` call on first send.
//!
//! Pure logic (repo ordering, folder-browser navigation, traits summary) lives
//! in free functions with unit tests; RPC results land in [`Loadable`] slots
//! rendered as skeletons / inline errors with Retry.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};

use cypher_engine::registry::HarnessDescriptor;
use cypher_proto::{
    ChatConfig, FolderListing, HarnessId, Model, ReasoningLevel, RepoRef, SandboxLevel, Space,
};
use cypher_rpc::methods;

/// Display cap for the ref list (t3code shows pages of 100 with a status
/// footer; a flat cap + "Showing X of Y refs" reads the same without
/// pagination plumbing).
const MAX_REF_ROWS: usize = 300;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::motion;
use crate::popover::{self, Loadable, MenuKey};
use crate::prefs::composer_defaults::ComposerDefaults;
use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;
mod pure;
pub use pure::*;
mod checkout;
mod keyboard;
mod loads;
mod render;
mod selections;
mod space;

// ---------------------------------------------------------------------------
// Catalog invalidation (Settings → Agents toggles)
// ---------------------------------------------------------------------------

/// Marker global: [`bump_harness_catalog`] pokes it whenever a Settings →
/// Agents toggle changes some device's enabled set, and every [`Pickers`]
/// observes it to force-refresh its cached harness catalog — without this the
/// composer served the boot-time list until restart (user report).
#[derive(Default)]
pub struct HarnessCatalogChanged;

impl gpui::Global for HarnessCatalogChanged {}

/// Notify all composers that some device's harness catalog changed. The
/// global carries no data — `set_global` notifies observers each time, and
/// they re-fetch from the engine (the source of truth).
pub fn bump_harness_catalog(cx: &mut App) {
    cx.set_global(HarnessCatalogChanged);
}

#[derive(Clone)]
pub enum PickerEvent {
    OpenAgentSettings { target_device: String },
}

impl gpui::EventEmitter<PickerEvent> for Pickers {}

fn missing_pi_runtime(harness: Option<HarnessId>, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    harness == Some(HarnessId::Pi)
        && message.contains("harness binary not found")
        && message.contains("runtime")
        && message.contains("not installed")
}

fn runtime_install_guidance(device: &str, remote: bool) -> String {
    if remote {
        format!(
            "Pi Runtime is not installed on the remote device “{device}” that runs this chat. \
             Open Settings → Agents, select “{device}”, and click Download runtime. \
             Installing it on this Mac will not install it on the remote device."
        )
    } else {
        format!(
            "Pi Runtime is not installed on “{device}”. \
             Open Settings → Agents and click Download runtime."
        )
    }
}

fn concrete_pi_model(id: &str) -> bool {
    id.split_once('/').is_some_and(|(provider, model)| {
        !provider.is_empty() && !model.is_empty() && provider != "unknown" && model != "unknown"
    })
}

/// Pi catalogs use `provider/model`. Mock has no prefix.
fn model_provider_id(harness: HarnessId, model_id: &str) -> String {
    if harness == HarnessId::Mock {
        return "mock".into();
    }
    model_id
        .split_once('/')
        .map(|(provider, _)| provider)
        .filter(|provider| !provider.is_empty() && *provider != "unknown")
        .unwrap_or("other")
        .to_string()
}

fn provider_display_name(id: &str) -> SharedString {
    SharedString::from(match id {
        "anthropic" | "claude-code" | "claude-bridge" => "Claude",
        "openai-codex" | "openai" => "ChatGPT",
        "mock" => "Mock",
        "other" => "Other",
        other => other,
    })
}

fn provider_brand_icon(id: &str) -> (&'static str, Option<gpui::Hsla>) {
    match id {
        "anthropic" | "claude-code" | "claude-bridge" => (
            crate::icons::CLAUDE_MARK,
            Some(crate::icons::claude_brand()),
        ),
        "openai-codex" | "openai" => (crate::icons::OPENAI_MARK, None),
        "mock" => (
            crate::icons::CLAUDE_MARK,
            Some(crate::icons::claude_brand()),
        ),
        _ => (crate::icons::GLOBAL, None),
    }
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

/// Sentinel for "no keyboard-highlighted row" (`active`): matches no index,
/// and `usize::MAX as isize == -1` — `menu_step` treats it like `None`, so
/// the first Down lands on row 0.
const NO_ACTIVE_ROW: usize = usize::MAX;

/// Which pane the model picker's icon rail is showing. `Provider` means
/// the models of [`Pickers::selected_provider`] (Pi `provider/model` prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ModelRail {
    Favorites,
    #[default]
    Provider,
}

/// One row of the model list. Search and favorites mix providers; the
/// subline names the provider (t3code ModelListRow `showProvider`).
#[derive(Debug, Clone)]
struct ModelRowData {
    harness: HarnessId,
    provider_id: String,
    provider_title: SharedString,
    model: Model,
}

/// Which picker popover is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Branch,
    /// The checkout-kind dropdown in the composer footer (Current
    /// checkout/worktree | New worktree).
    Checkout,
    HarnessModel,
    Traits,
    /// New-session canvas only: which project the session mints into. A pick
    /// re-keys everything project-derived (refs, harness/model catalogs) via
    /// the state observer.
    Space,
    /// New-session canvas only: the device project-less sessions run on (a
    /// project pick implies its own host and overrides this).
    Device,
}

pub struct Pickers {
    state: Entity<AppState>,
    /// A temporary Side Chat's composer: the model/traits chips start on the
    /// inherited values and picks stamp the fork's synthetic row LOCALLY —
    /// never `setChatConfig` (the row is engine-owned until promotion, which
    /// carries the picked config over). Checkout/ref/project chips stay inert.
    side_chat: bool,
    /// The composer is narrow (a small tile): the Traits chip steps aside so
    /// the model chip keeps a readable label ([`Self::set_narrow`]).
    narrow: bool,
    config: DraftConfig,
    /// Sticky last-used picks (zeron `zeron.composer.defaults:v1`): seeds the
    /// new-chat chips and is rewritten on every new-chat pick.
    defaults: ComposerDefaults,
    /// Where [`Self::defaults`] persists (`{data_dir}/composer-defaults.json`);
    /// `None` before bootstrap stamps the state (writes are skipped).
    data_dir: Option<PathBuf>,
    /// Selection the draft picks belong to — switching chats drops them so a
    /// pick made in one chat never leaks into another.
    draft_owner: Option<String>,
    /// Space the branch draft/cache belong to (see the state observer).
    space_owner: Option<String>,
    /// Programmatic checkout target (the sidebar's hover add buttons) —
    /// space-scoped and authoritative without refs; see [`PinnedCheckout`].
    pinned: Option<PinnedCheckout>,
    open: popover::Popup<PickerKind>,
    /// The model picker's rail selection (favorites vs one provider).
    /// Re-primed on every open.
    model_rail: ModelRail,
    /// Provider id (`anthropic`, `openai-codex`, a custom gateway, or `mock`)
    /// shown when [`ModelRail::Provider`] is selected.
    selected_provider: Option<String>,
    harnesses: Loadable<Vec<HarnessDescriptor>>,
    models: HashMap<HarnessId, Loadable<Vec<Model>>>,
    model_generation: u64,
    catalog_owner: Option<String>,
    refs: Loadable<Vec<RepoRef>>,
    /// Space id the `refs` slot belongs to (invalidated on space change).
    refs_space: Option<String>,
    /// Highlighted row in the open list (keyboard nav).
    active: usize,
    /// Models-list scroll — keyboard nav keeps the highlighted row in view
    /// (`scroll_to_item`; the add-space palette standard).
    model_scroll: gpui::ScrollHandle,
    /// Shared search / URL / name input, reused across popovers.
    search: Entity<ComposerInput>,
    /// One-shot mute for the next Edited event's highlight reset — armed by
    /// [`Self::toggle`]'s programmatic clear (see the subscription).
    search_reset_muted: bool,
    focus: FocusHandle,
    load_task: Option<Task<()>>,
    /// Own slot: the refs load runs concurrently with the eager
    /// harness/model loads — sharing `load_task` would abort one mid-flight.
    refs_task: Option<Task<()>>,
    /// In-flight mid-session `SwitchRef` (the ref being switched to).
    switching: Option<String>,
    switch_task: Option<Task<()>>,
    /// Last mid-session switch failure (shown in the ref popover).
    switch_error: Option<String>,
    mutate_task: Option<Task<()>>,
    _search_events: Subscription,
    _state_observe: Subscription,
    _catalog_observe: Subscription,
}

impl Pickers {
    /// Composer-driven width gate (see `narrow`); the chips also ellipsize
    /// under row pressure either way.
    pub fn set_narrow(&mut self, narrow: bool, cx: &mut Context<Self>) {
        if self.narrow != narrow {
            self.narrow = narrow;
            cx.notify();
        }
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| ComposerInput::new("Search…", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Edited => {
                // Typing in a filter resets the highlight to the top of the
                // fresh results. `set_text` emits Edited on programmatic
                // clears too, and this subscription runs AFTER `toggle`
                // returns — an unmuted reset clobbers the just-anchored
                // selected row back to 0, leaving the top row wearing a
                // second highlight next to the selection (user report;
                // `toggle` arms the mute right before its clear).
                if !std::mem::take(&mut this.search_reset_muted) {
                    if this.open_kind() == Some(PickerKind::Branch) {
                        this.active = 0;
                    }
                    if this.open_kind() == Some(PickerKind::HarnessModel) {
                        this.active = 0;
                        this.model_scroll.set_offset(gpui::Point::default());
                    }
                }
                cx.notify();
            }
            ComposerInputEvent::Submitted => this.on_search_submit(cx),
            // Pasted images/files don't apply to a search box.
            ComposerInputEvent::PastedImages(_)
            | ComposerInputEvent::PastedPaths(_)
            | ComposerInputEvent::CursorMoved
            | ComposerInputEvent::ViewportChanged
            | ComposerInputEvent::MentionNavigate(_)
            | ComposerInputEvent::MentionAccept
            | ComposerInputEvent::MentionDismiss => {}
        });
        // Chat selection / config changes must re-render the chips (child views
        // only re-render on their own notify). A selection change also drops
        // the draft picks — they belonged to the previous chat/new-chat canvas.
        // The transitions are delegated to [`Self::sync_draft_owner`] /
        // [`Self::sync_space_owner`] — the SAME helpers `target_checkout` runs
        // synchronously — so a deferred run here never re-clears a fresh pin
        // whose owners were already synchronized (see `target_checkout`).
        let state_observe = cx.observe(&state, |this: &mut Self, state, cx| {
            let state = state.read(cx);
            this.sync_draft_owner(state.selected_chat.clone());
            this.sync_space_owner(state.selected_space.clone());
            // An explicit no-project opt-out INVALIDATES the programmatic
            // checkout pin even when the raw selected-space id did not change
            // (the opt-out leaves the id in place): the pin must be cleared,
            // not merely masked by `pinned_plan` — a later re-pick of the same
            // project must not revive it.
            if Self::no_project_invalidates_pin(this.pinned.is_some(), state.no_project) {
                this.clear_pinned_target();
            }
            this.sync_catalog_owner(cx);
            cx.notify();
        });
        // A Settings → Agents toggle changed some device's enabled set:
        // force-refresh the cached catalog so the rail/chips follow without a
        // restart (stale rows stay visible while the reload runs).
        let catalog_observe = cx.observe_global::<HarnessCatalogChanged>(|this: &mut Self, cx| {
            this.model_generation = this.model_generation.wrapping_add(1);
            this.models.clear();
            // Cancel an older in-flight catalog request as well. Otherwise its
            // generation is stale, but Loading prevents the fresh request.
            this.load_task = None;
            this.harnesses = Loadable::Idle;
            this.ensure_harnesses(true, cx);
            cx.notify();
        });
        // Sticky last-used picks: loaded synchronously so the very first frame
        // shows the remembered harness/model/reasoning, never a placeholder.
        let data_dir = state.read(cx).data_dir.clone();
        let defaults = data_dir
            .as_deref()
            .map(ComposerDefaults::load)
            .unwrap_or_default();
        // Restore the last device/project picks (the canvas's "defaults to
        // last selected" rule). Vanished rows heal in `apply_spaces`. A
        // remembered "Don't work in a project" opt-out is deliberately NOT
        // restored: the menu row is gone, so a stale saved opt-out would
        // strand the canvas in a state the picker can no longer express.
        {
            let device = defaults.device.clone();
            let project = defaults.project.clone();
            state.update(cx, |s, _| {
                if s.selected_device.is_none() {
                    s.selected_device = device;
                }
                if s.selected_space.is_none() {
                    s.selected_space = project;
                }
            });
        }
        let draft_owner = state.read(cx).selected_chat.clone();
        let space_owner = state.read(cx).selected_space.clone();
        Self {
            state,
            side_chat: false,
            narrow: false,
            space_owner,
            config: DraftConfig::default(),
            defaults,
            data_dir,
            draft_owner,
            open: popover::Popup::default(),
            model_rail: ModelRail::default(),
            selected_provider: None,
            harnesses: Loadable::Idle,
            models: HashMap::new(),
            model_generation: 0,
            catalog_owner: None,
            refs: Loadable::Idle,
            refs_space: None,
            pinned: None,
            active: 0,
            model_scroll: gpui::ScrollHandle::new(),
            search,
            search_reset_muted: false,
            focus: cx.focus_handle(),
            load_task: None,
            refs_task: None,
            switching: None,
            switch_task: None,
            switch_error: None,
            mutate_task: None,
            _search_events: search_events,
            _state_observe: state_observe,
            _catalog_observe: catalog_observe,
        }
    }

    /// Apply one change to the sticky defaults and persist it (best-effort;
    /// picks are rare and tiny). Every tile's composer holds its own copy, so
    /// saving that copy whole would drop picks other tiles made since it
    /// loaded: the file is re-read, only `change` is applied on top, and the
    /// merged result refreshes this copy.
    fn update_defaults(&mut self, change: impl FnOnce(&mut ComposerDefaults)) {
        let Some(dir) = self.data_dir.as_deref() else {
            change(&mut self.defaults);
            return;
        };
        let mut merged = ComposerDefaults::load(dir);
        change(&mut merged);
        if let Err(err) = merged.save(dir) {
            tracing::warn!(error = %err, "composer-defaults save failed");
        }
        self.defaults = merged;
    }

    /// Bind the pickers to a temporary Side Chat's fork: only the model and
    /// traits popovers open, and their picks stay on the fork's synthetic row
    /// ([`Self::update_chat_config`]).
    pub fn set_side_chat(&mut self) {
        self.side_chat = true;
    }

    /// Harness is locked once the chat exists.
    fn harness_locked(&self, cx: &App) -> bool {
        self.state.read(cx).selected_chat.is_some()
    }

    fn engine(&self, cx: &App) -> Option<EngineHandle> {
        self.state.read(cx).engine().cloned()
    }

    /// The catalog host device when it differs from the connected engine's
    /// own — harness/model catalogs come from the device that RUNS the agents
    /// (the CLIs live there; the viewer may have neither claude nor codex
    /// installed — user report: "can't load codex models/traits anywhere"
    /// from a Mac without codex). Prefers the selected chat's OWN host device
    /// (a side chat's synthetic row names its parent's device — including a
    /// remote PROJECT-LESS side chat, which has no space row to target); a
    /// new-chat canvas targets its picked project's host.
    fn sync_catalog_owner(&mut self, cx: &App) {
        let owner = self
            .space_target(cx)
            .or_else(|| self.state.read(cx).local_device_id.clone());
        if owner != self.catalog_owner {
            self.catalog_owner = owner;
            self.model_generation = self.model_generation.wrapping_add(1);
            self.models.clear();
            self.harnesses = Loadable::Idle;
            self.load_task = None;
        }
    }

    fn space_target(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let device = state
            .selected_chat_row()
            .map(|c| c.device_id.clone())
            .or_else(|| state.selected_space_row().map(|s| s.device_id.clone()))
            // A quick-chat canvas has no project: its models come from the
            // device the chat will run on.
            .or_else(|| {
                state
                    .scratch_pending
                    .then(|| state.effective_device_id())
                    .flatten()
            })?;
        (state.local_device_id.as_deref() != Some(device.as_str())).then_some(device)
    }

    /// Effective harness: picked, or the chat's config, or the first listed.
    fn effective_harness(&self, cx: &App) -> Option<HarnessId> {
        if let Some(harness) = self.config.harness {
            return Some(harness);
        }
        if let Some(config) = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.config.as_ref())
        {
            return Some(config.harness);
        }
        // New-chat canvas: the remembered last-used harness (sticky defaults),
        // when the loaded catalog still offers it (the device may have
        // disabled it in Settings → Agents since).
        if let Some(harness) = self.defaults.harness {
            let offered = match self.harnesses.ready() {
                Some(list) => offered_harnesses(list).iter().any(|d| d.id == harness),
                None => true, // catalog not loaded yet — trust the memory
            };
            if offered {
                return Some(harness);
            }
        }
        // Fall back to the first OFFERED harness: the registry lists the mock
        // harness first, and resolving chips against it would boot the
        // new-chat canvas onto "Mock" instead of Pi + its default model (it
        // stays available under `CYPHER_HARNESS=mock`).
        self.harnesses
            .ready()
            .and_then(|list| offered_harnesses(list).first().map(|d| d.id))
    }

    /// Effective model id: the draft pick, the selected chat's config, or (on
    /// the new-chat canvas) the remembered last-used model for the harness.
    fn effective_model_id<'a>(&'a self, cx: &'a App) -> Option<&'a str> {
        if let Some(id) = self.config.model.as_deref() {
            return Some(id);
        }
        if let Some(chat) = self.state.read(cx).selected_chat_row() {
            return chat.config.as_ref().and_then(|c| c.model.as_deref());
        }
        let harness = self.effective_harness(cx)?;
        self.defaults.model_for(harness).map(|m| m.id.as_str())
    }

    /// Effective reasoning — always concrete once the model is known: the
    /// draft pick / chat config / remembered default, clamped to the selected
    /// model's ladder, falling back to the model's default level.
    fn effective_reasoning(&self, cx: &App) -> Option<ReasoningLevel> {
        let explicit = self.config.reasoning.or_else(|| {
            match self.state.read(cx).selected_chat_row() {
                Some(chat) => chat.config.as_ref().and_then(|c| c.reasoning),
                // New chat: the remembered last-used level.
                None => self.defaults.reasoning,
            }
        });
        if self.selected_model(cx).is_none() {
            // Catalog not loaded yet: show the explicit value as-is (nothing
            // to clamp against); it resolves to a concrete level on load.
            return explicit;
        }
        clamp_reasoning(explicit, &self.trait_ladder(cx))
    }

    /// The selected model — concrete from the moment the list loads: the
    /// effective id when the list still offers it, else the harness default.
    /// Pi keeps a missing explicit selection unresolved rather than silently
    /// sending the next prompt to a different provider.
    fn selected_model<'a>(&'a self, cx: &'a App) -> Option<&'a Model> {
        let harness = self.effective_harness(cx)?;
        let models = self.models.get(&harness)?.ready()?;
        match self.effective_model_id(cx) {
            Some(id) if harness == HarnessId::Pi && concrete_pi_model(id) => {
                models.iter().find(|m| m.id == id)
            }
            Some(id) => models
                .iter()
                .find(|m| m.id == id)
                .or_else(|| default_model(models)),
            None => default_model(models),
        }
    }

    /// Removing credentials or a provider must not silently select a different
    /// service for the next user message.
    pub fn unavailable_pi_model<'a>(&'a self, cx: &'a App) -> Option<&'a str> {
        if self.effective_harness(cx) != Some(HarnessId::Pi) {
            return None;
        }
        let id = self.effective_model_id(cx)?;
        let models = self.models.get(&HarnessId::Pi)?.ready()?;
        (concrete_pi_model(id) && !models.iter().any(|m| m.id == id)).then_some(id)
    }

    /// The explicit (non-default) option picks: the chat's persisted
    /// selections for existing chats, the draft's for the new-chat canvas.
    fn explicit_options(&self, cx: &App) -> serde_json::Map<String, serde_json::Value> {
        match self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.config.as_ref())
        {
            Some(config) => config.model_options.clone(),
            None => self.config.model_options.clone(),
        }
    }

    /// The fully-resolved config the composer threads into the Run request and
    /// `Mutate createChat`: concrete model + reasoning whenever the catalog is
    /// loaded (no "engine picks a default" passthrough).
    /// The resolved harness's steering mode, from the loaded descriptor list.
    /// `None` while the catalog is loading (callers should assume the common
    /// StepBoundary case and show nothing).
    pub fn resolved_steering_mode(&self, cx: &App) -> Option<cypher_proto::SteeringMode> {
        let harness = self.effective_harness(cx)?;
        self.harnesses
            .ready()
            .and_then(|list| list.iter().find(|d| d.id == harness))
            .map(|d| d.steering_mode)
    }

    pub fn resolved(&self, cx: &App) -> ResolvedRunConfig {
        ResolvedRunConfig {
            harness: self.effective_harness(cx),
            model: self
                .selected_model(cx)
                .map(|m| m.id.clone())
                // Catalog not loaded (offline): still send the id we know.
                .or_else(|| self.effective_model_id(cx).map(str::to_string)),
            reasoning: self.effective_reasoning(cx),
            model_options: self.explicit_options(cx),
        }
    }

    // ---- open/close ----

    /// The picker that's open AND interactive — `None` while one animates out.
    fn open_kind(&self) -> Option<PickerKind> {
        self.open.as_open().copied()
    }

    /// The picker to render: open or mid-exit.
    fn mounted_kind(&self) -> Option<PickerKind> {
        self.open.get().copied()
    }

    /// Begin the exit animation (shared by every close path).
    fn animate_close(&mut self, cx: &mut Context<Self>) {
        if self.open.begin_close() {
            popover::reap_popup(cx, |pickers: &mut Self| &mut pickers.open);
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.animate_close(cx);
        cx.notify();
    }

    /// Capture knob (`CYPHER_OPEN_DIALOG=model`): open the combined
    /// harness/model menu programmatically.
    pub fn open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_kind() != Some(PickerKind::HarnessModel) {
            self.toggle(PickerKind::HarnessModel, window, cx);
        }
    }

    fn toggle(&mut self, kind: PickerKind, window: &mut Window, cx: &mut Context<Self>) {
        // Temporary Side Chat: only the model/traits chips are live — its
        // checkout and target are the parent's, fixed.
        if self.side_chat && !matches!(kind, PickerKind::HarnessModel | PickerKind::Traits) {
            return;
        }
        // A press that found this picker open closes it — the card's
        // `on_mouse_down_out` already began the close on that same press,
        // so by click time the popup reads as closed and a plain toggle
        // would reopen it. A press while a DIFFERENT picker is open doesn't
        // count (see note_trigger_press_matching): that click switches.
        let pressed_open = self.open.take_press_was_open();
        if self.open_kind() == Some(kind) || pressed_open {
            self.animate_close(cx);
            cx.notify();
            return;
        }
        self.open.open(kind);
        // Clearing stale text emits Edited AFTER this function returns —
        // mute that one event so its reset can't clobber the highlight
        // anchored below (the no-op clear is also skipped for the same
        // reason).
        self.search_reset_muted = !self.search.read(cx).text().is_empty();
        self.search.update(cx, |input, cx| {
            input.set_placeholder("Search…", cx);
            if !input.text().is_empty() {
                input.set_text("", cx);
            }
        });
        // Prime the model picker's rail BEFORE anchoring the highlight (the
        // visible rows depend on it): the favorites view when stars exist —
        // t3 ModelPickerContent's initial selection — else the effective
        // harness. Locked chats stay on their own harness.
        if kind == PickerKind::HarnessModel {
            // A failure is not a permanent catalog: the remote device may
            // have installed its Runtime or connected a provider elsewhere.
            self.models
                .retain(|_, slot| !matches!(slot, Loadable::Error(_)));
            // Neither is a success: the host may have taken a background
            // Runtime update that bundles a new provider (pi-claude-bridge)
            // since this list was cached — nothing on the desktop observes
            // a remote host's Runtime version, so revalidate on open.
            self.revalidate_ready_models(cx);
            self.prefetch_models(cx);
            self.model_rail = if !self.harness_locked(cx) && !self.defaults.favorites.is_empty() {
                ModelRail::Favorites
            } else {
                ModelRail::Provider
            };
            if self.selected_provider.is_none() {
                self.selected_provider = self.viewed_provider(cx);
            }
        }
        // The keyboard-nav highlight starts ON the selected row — row 0
        // otherwise reads as a second active row (user report).
        self.active = match kind {
            PickerKind::Checkout => match self.config.checkout {
                CheckoutKind::Local => 0,
                CheckoutKind::NewWorktree => 1,
            },
            PickerKind::Branch => self.selected_ref_index(cx),
            PickerKind::HarnessModel | PickerKind::Traits => self.selected_model_index(cx),
            PickerKind::Space => self.selected_space_index(cx),
            PickerKind::Device => self.selected_device_index(cx),
        };
        if kind == PickerKind::HarnessModel {
            self.model_scroll.set_offset(gpui::Point::default());
            self.model_scroll.scroll_to_item(self.active);
        }
        // Searchable pickers focus the filter input (it sits inside the frame,
        // so the frame's key handler still sees arrows/Enter); the rest focus
        // the frame itself for pure keyboard nav.
        match kind {
            PickerKind::Branch => {
                self.switch_error = None; // stale mid-session failures don't linger
                let handle = self.search.read(cx).focus_handle(cx);
                self.search.update(cx, |input, cx| {
                    input.set_placeholder("Search refs…", cx);
                });
                window.focus(&handle, cx);
            }
            PickerKind::Space => {
                let handle = self.search.read(cx).focus_handle(cx);
                self.search.update(cx, |input, cx| {
                    input.set_placeholder("Search projects…", cx);
                });
                window.focus(&handle, cx);
            }
            PickerKind::Device => {
                let handle = self.search.read(cx).focus_handle(cx);
                self.search.update(cx, |input, cx| {
                    input.set_placeholder("Search devices…", cx);
                });
                window.focus(&handle, cx);
            }
            PickerKind::HarnessModel => {
                let handle = self.search.read(cx).focus_handle(cx);
                self.search.update(cx, |input, cx| {
                    input.set_placeholder("Search models…", cx);
                });
                window.focus(&handle, cx);
            }
            _ => window.focus(&self.focus, cx),
        }
        match kind {
            // Force: the checkout state moves under us (a send mints a
            // worktree+branch, terminals switch refs) — every open
            // revalidates, keeping stale rows visible until fresh ones land.
            PickerKind::Branch | PickerKind::Checkout => self.ensure_refs(true, cx),
            PickerKind::HarnessModel | PickerKind::Traits => {
                // Force: the enabled set moves under us (Settings → Agents,
                // possibly from another viewer) — every open revalidates,
                // keeping current rows visible until the fresh catalog lands.
                self.ensure_harnesses(true, cx);
                self.prefetch_models(cx);
            }
            // Projects and devices are already synced state — nothing to load.
            PickerKind::Space | PickerKind::Device => {}
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
