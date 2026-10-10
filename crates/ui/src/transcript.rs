//! The conversation view: virtualized transcript with block-granularity rows,
//! stick-to-bottom, tool-group folding, and streaming markdown.
//!
//! Row model:
//! - one row per BLOCK: user message = one bubble row; assistant messages split
//!   into one row per markdown top-level block, plus consecutive-tool groups and
//!   input/error chips;
//! - stable row ids `{msgId}#{partId}.{blockIx}` / `{msgId}#g{groupIx}` — LIVE
//!   (streaming) entries split per block exactly like completed ones (the list
//!   virtualizes them, so a fading live reply re-renders only its visible tail
//!   each frame — flat cost in the reply length); on completion each block row
//!   keeps its id, so row identity is continuous and nothing flickers;
//! - rows are cached per entry keyed by a content fingerprint — only changed
//!   messages rebuild (the anti-"streaming stutter" trick);
//! - row-set changes diff by (id, version) into one minimal `splice`.
//!
//! Stick-to-bottom is a velocity spring (ported from mugen, the same shape as
//! stackblitz's use-stick-to-bottom): while pinned, a per-frame stepper glides
//! the viewport toward the list end with a feed-forward term tracking the
//! smoothed target growth, so 120ms doc commits read as a continuous glide
//! instead of per-commit snaps. The pin breaks only on user input (the list's
//! scroll handler fires exclusively from its wheel/touch path) and re-engages
//! inside the 70px band; the first send in an empty chat anchors the prompt at
//! the viewport top and hands off to the same glide when the reply overflows.
//! While that anchor holds, wheel/touch is clamped rather than obeyed — the
//! whole turn is already visible, so there is nothing to scroll to.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, BorderStyle, ClipboardItem, Context, Entity, EventEmitter, KeyBinding,
    ListAlignment, ListOffset, ListScrollEvent, ListState, ObjectFit, SharedString,
    StyledImage as _, StyledText, Subscription, Task, TextRun, Window, actions, canvas, div, img,
    list, prelude::*, px, quad,
};

use cypher_doc::{MessageComment, MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use cypher_proto::view::Indicator;
use cypher_proto::{AnsweredModel, Chat, HarnessId, ToolCall};

use crate::markdown::parser::{Block, BlockTree, IncrementalParser, parse_full};
use crate::markdown::render::{self, RenderCache, RenderOptions};
use crate::markdown::veil::RowVeil;
use crate::motion::{self, AnimationExt as _, RESIZE};
use crate::state::AppState;
use crate::syntax_cache::{DocumentHighlightKey, SyntaxHighlightCache};
use crate::theme::{MonoStyled, Theme};
use cypher_syntax::LanguageId as Lang;
mod spring;
pub use spring::*;
mod rows;
pub use rows::*;
mod chips;
pub use chips::*;
mod highlight;
use highlight::*;
mod attachments;
mod find;
pub mod rail;
mod rendering;
mod scroll;
use rendering::*;
mod comments;

/// Key context of a transcript holding focus (a click into the chat history).
pub const KEY_CONTEXT: &str = "Transcript";

actions!(transcript, [PrevPrompt, NextPrompt]);

/// Bind the transcript keymap: ↑/↓ step between the user's prompts while the
/// chat history holds focus. Call at boot and on every keymap re-apply.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", PrevPrompt, Some(KEY_CONTEXT)),
        KeyBinding::new("down", NextPrompt, Some(KEY_CONTEXT)),
    ]);
}

// ---------------------------------------------------------------------------
// Constants (mugen ports)
// ---------------------------------------------------------------------------

/// Re-engage the bottom pin when the user returns within this many px of the end.
pub const STICK_THRESHOLD_PX: f32 = 70.0;
/// List overdraw beyond the viewport.
pub const OVERDRAW_PX: f32 = 320.0;
/// Show the scroll-to-bottom button beyond this distance from the end.
pub const SCROLL_BUTTON_THRESHOLD_PX: f32 = 320.0;
/// How long a rewind affordance stays ARMED after its first click. Restarting
/// deletes messages permanently, so the confirming click has to be deliberate
/// — and a button left hot forever would make the next stray click destructive.
pub const REWIND_ARM_MS: u64 = 4000;
/// Vertical gap opening a new turn (new message entry) in the default style.
#[cfg(test)]
pub const GAP_TURN: f32 = 14.0;
/// Vertical gap between blocks within a turn.
pub const GAP_BLOCK: f32 = 8.0;
/// Transcript column max width (zeron 46rem).
pub const MAX_CONTENT_WIDTH: f32 = crate::chat_style::CONTENT_WIDTH;
/// Tool chip row height / gap — analytic, so fold heights need no measurement.
/// A row is the guide rail + a 30px chip card centered in it (zeron
/// tool-chip.tsx: `TOOL_CHIP_HEIGHT = 38`, card `h-[30px]`); rows stack with no
/// gap so the rail reads continuous.
pub const CHIP_HEIGHT: f32 = 38.0;
pub const CHIP_GAP: f32 = 0.0;
pub const CHIP_CARD_HEIGHT: f32 = 30.0;
const CHIPS_TOP_PAD: f32 = 2.0;
/// How long a user fold toggle keeps its height tween armed: the RESIZE
/// spec's 200ms plus margin. Past this the fold renders statically — an armed
/// tween replays on remount, i.e. on every scroll-back-into-view.
const FOLD_TWEEN_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
/// User-bubble attachment thumbnails (user-attachments.tsx): 112×80 thumbs in
/// a FIXED-height strip (load-state flips never shift the virtualizer).
pub const ATT_THUMB_W: f32 = 112.0;
pub const ATT_THUMB_H: f32 = 80.0;
pub const ATT_STRIP_H: f32 = ATT_THUMB_H + 10.0;

// ---------------------------------------------------------------------------
// Transcript entity
// ---------------------------------------------------------------------------

struct CachedRows {
    fingerprint: u64,
    rows: Vec<Row>,
}

#[derive(Default, Clone, Copy)]
struct FoldState {
    /// User pin (click); `None` follows the auto-open rule.
    open: Option<bool>,
    /// Bumped per toggle — keys the 200ms height tween.
    epoch: usize,
    /// Height at the moment of the toggle (the tween's start). The destination
    /// is always the *current* target height, so content growth after a toggle
    /// snaps instead of replaying a stale tween.
    from: f32,
    /// When the toggle happened. The tween is armed only for a short window
    /// after the click: gpui replays an element's animation on REMOUNT, and a
    /// virtualized row scrolling back into view is a remount — an armed-forever
    /// tween made every once-collapsed group flash open→closed on each
    /// reappearance (user report).
    toggled_at: Option<Instant>,
}

/// Layout state for the most recent locally-sent turn (notes-app parity):
/// EVERY send reserves the space below the prompt for the reply — a trailing
/// runway pad sized `usable − turn height`, i.e. a min-height for the turn,
/// shrinking 1:1 as the reply streams so the held layout never moves. The
/// entry is an eased glide onto the prompt; landed, the hold re-asserts the
/// prompt's position absolutely after every layout (the bottom spring can't
/// hold here: parking at exact distance 0 re-glues gpui's list, which then
/// hard-tracks the pad's stale bottom on every commit — rig-traced). Wheel
/// input releases the hold, leaving the reservation as plain scrollable
/// space. The anchor retires once the reply overflows the reservation (pad
/// ~0, height-neutral) and on explicit navigation / chat switches (revisits
/// start at the bottom).
struct OwnTurnAnchor {
    chat_id: String,
    message_id: SharedString,
    /// Current reservation pad on the last row (`usable − turn_height`).
    runway: f32,
    /// The step still owns the viewport (glide → hold). Any wheel/touch
    /// input releases it — the reservation stays behind as plain scrollable
    /// space, and the ordinary escape/restick rules apply from then on.
    held: bool,
    /// The entry glide has landed; the hold now re-asserts the prompt's
    /// position absolutely after every layout (glue- and lag-proof — the
    /// exact mechanism the shipped first-send anchor used).
    positioned: bool,
}

/// The transcript is a pure viewer now — comments live in the shared
/// [`crate::comments::CommentPopup`] (shell-rendered, shell-subscribed), so
/// the transcript no longer emits events of its own.
pub struct Transcript {
    state: Entity<AppState>,
    list: ListState,
    rows: Vec<Row>,
    chat_id: Option<String>,
    row_cache: HashMap<String, CachedRows>,
    live_parsers: HashMap<String, IncrementalParser>,
    tree_cache: HashMap<String, (usize, Arc<BlockTree>)>,
    folds: HashMap<SharedString, FoldState>,
    /// Detail folds (output/diff) per chip, keyed `"{row_id}#d{ix}"` — full
    /// [`FoldState`]s so detail bodies tween open/closed exactly like the
    /// group fold. Render-local like `folds` — never part of the row
    /// fingerprint.
    tool_details: HashMap<SharedString, FoldState>,
    /// Tool groups whose capped-away chips the user revealed, by row id. The
    /// cap itself is a setting (`chat_style::tool_call_limit`); this is the
    /// per-row override, render-local like `folds`.
    tool_overflow: std::collections::HashSet<SharedString>,
    /// Toggles the user clicked (an append-mode translation's original, a
    /// thought, a work run) and the state they left them in, by row id;
    /// every other one keeps its default ([`fold_closed_toggles`]).
    /// Render-local like `folds`.
    toggle_pins: HashMap<SharedString, bool>,
    /// Streaming fade veils, one per live markdown row (dropped on completion).
    veils: HashMap<SharedString, Rc<RefCell<RowVeil>>>,
    /// Live rows present in the transcript's REPLAY after (re)attaching to a
    /// chat: their veils are created pre-seeded, so text that was already
    /// streamed before the switch never fades in — only appends after it do
    /// (mugen's `FadePainter.attach` baseline; user report: switching back to
    /// a streaming session dissolved the entire reply).
    veil_baseline: std::collections::HashSet<SharedString>,
    /// Armed at attach, disarmed on the first sync whose transcript is
    /// non-empty: the baseline must be captured from the doc REPLAY frame,
    /// not the attach-time sync — selection clears the transcript and the
    /// replay lands async, so capturing at attach seeded nothing and the
    /// still-streaming reply faded in whole on every session switch (user
    /// report).
    veil_attach_pending: bool,
    /// Cross-frame flatten/shape-input cache (see [`RenderCache`]): fade
    /// frames reuse settled blocks' text+runs; the incremental parser's stable
    /// boundary invalidates only the live tail per commit.
    render_cache: Rc<RefCell<RenderCache>>,
    highlights: HighlightStore,
    show_jump_button: bool,
    /// Vertical delta (px, positive = toward older content) of the wheel
    /// event being dispatched, recorded in the capture phase so the list's
    /// scroll handler can tell which way the USER moved — escape and restick
    /// are direction-aware (see [`Transcript::should_restick`]).
    wheel_dy: Rc<std::cell::Cell<f32>>,
    /// The stick-to-bottom pin. Broken only by user input (wheel/touch up);
    /// re-engaged inside the 70px band, after an own-send first overflows, and
    /// on the jump button.
    pinned: bool,
    /// A locally-sent prompt currently held near the viewport top while its
    /// reply grows into the empty space below it.
    own_turn: Option<OwnTurnAnchor>,
    /// A layout-affecting change needs one post-layout own-turn measurement.
    own_turn_kick: bool,
    /// One own-turn `on_next_frame` callback in flight at most.
    own_turn_scheduled: bool,
    /// Wall-clock of the previous entry-glide tick (`None` = not gliding).
    own_turn_last_tick: Option<Instant>,
    spring: StickSpring,
    /// Wall-clock of the previous spring tick (`None` = parked).
    spring_last_tick: Option<Instant>,
    /// When the spring last landed on the bottom (settle-grace bookkeeping).
    spring_settled_at: Option<Instant>,
    /// A doc commit / wake happened before layout measured it — run at least
    /// one spring tick even though the pre-layout distance still reads 0.
    spring_kick: bool,
    /// One `on_next_frame` callback in flight at most.
    spring_scheduled: bool,
    scroll_anim: Option<Task<()>>,
    /// Keyboard focus for the chat history: a click anywhere in it lands here,
    /// so ↑/↓ ([`KEY_CONTEXT`]) step between prompts.
    focus: gpui::FocusHandle,
    /// The prompt row a ↑/↓ glide is heading to, and when it set off. Held
    /// key-repeat steps on from here rather than from the mid-glide viewport,
    /// which would keep re-targeting the same prompt.
    prompt_nav: Option<(usize, Instant)>,
    /// MessageRail width gate (set by the shell from the container width).
    rail_enabled: bool,
    /// Selection scope this transcript paints into: the shared Transcript
    /// scope for the main surface, a FRESH [`SelectionScope::SideChat`] id per
    /// temporary panel so a side chat beside the main transcript (or two side
    /// chats) never collide in the selection registry.
    scope: crate::markdown::selection::SelectionScope,
    /// Embedded (temporary Side Chat) layout: narrow-panel row gutters and a
    /// smaller first-row top inset instead of the main surface's titlebar
    /// chrome gap. Also disables the annotation surface (no Comment pill, no
    /// nested Side Chat) while selection + copy stay active.
    embedded: bool,
    /// Height of the shell's composer/status/terminal stack overlaying the
    /// transcript's bottom (measured last frame): the last row pads past it
    /// so pinned content rests above the glass chrome it scrolls under.
    bottom_clearance: f32,
    /// The shell-reported viewport size (the tile's chat column); a change
    /// re-anchors the list ([`Self::set_viewport_size`]). `None` until the
    /// first report.
    viewport_size: Option<(f32, f32)>,
    /// `(state revision, selected chat)` the rows were last built from —
    /// [`Self::sync`] skips the transcript clone and row rebuild when a
    /// notify changed neither.
    /// (The attachment devices ride along: protected attachments re-key when
    /// the chat's row or the local device id lands; so does the tool call
    /// cap, which [`cap_work_runs`] applies at build time.)
    synced_revision: Option<(u64, Option<String>, Vec<String>, u32)>,
    /// Hovered rail tick (grows + shows the preview card).
    rail_hover: Option<usize>,
    /// `(row id, entry id)` under the pointer — reveals the entry's timestamp
    /// strip (zeron chat-view.tsx `group-hover`; the rows report hover
    /// themselves). Keyed by ROW so a row→row move within one entry can't
    /// clear the reveal when the old row's leave event arrives after the new
    /// row's enter (enter/leave order across rows is not guaranteed).
    hovered_entry: Option<(SharedString, SharedString)>,
    /// Code block showing "Copied" feedback: `(row id, block ix)`, cleared by
    /// the companion task after ~1.2s.
    copied_code: Option<(SharedString, usize)>,
    copied_clear: Option<Task<()>>,
    /// Entry-level copy feedback is separate from a code block's copy state.
    copied_message: Option<SharedString>,
    copied_message_clear: Option<Task<()>>,
    /// Transcript attachment being viewed full-size (click a user thumbnail).
    attachment_preview: Option<crate::attachments::PreviewImage>,
    /// Focused while the lightbox is open so Escape reaches it.
    attachment_preview_focus: gpui::FocusHandle,
    /// In-flight ReadAttachmentChunk loads, keyed `(deviceId, path)` — one per
    /// source; results land in the global attachment cache.
    attachment_loads: HashMap<(String, String), Task<()>>,
    /// Scheduled retry wake-ups for errored sources (the 2s→15s ladder).
    attachment_retries: HashMap<(String, String), Task<()>>,
    /// Sidecar blob fetches keyed by doc ref (`chatId/partId[.diff]`,
    /// chat2-sync A3). `Ready` holds the UPGRADED detail, built once on
    /// arrival — render swaps it in per chip; rows never rebuild for it.
    /// Deliberately NOT cleared on chat switch: refs are chat-qualified and a
    /// fetched blob stays valid.
    blob_details: HashMap<SharedString, BlobFetch>,
    /// Monotonic fetch order per blob ref: when a tool has BOTH a diff and
    /// an output blob fetched, the chip shows the one requested most
    /// recently (click "Show full output" after a diff → see the output).
    blob_fetch_order: HashMap<SharedString, u64>,
    blob_fetch_counter: u64,
    /// The shared shell-level Comment pill/editor. Weak: the
    /// shell owns it; the transcript only ever drives and reads it.
    comment_popup: gpui::WeakEntity<crate::comments::CommentPopup>,
    /// In-flight Session Forks, keyed `(chat id, anchor message id)`: while
    /// an entry's fork RPC is pending its affordance shows a spinner and is
    /// inert (double-click guard). The shell begins/ends these around the
    /// ForkSession call.
    fork_pending: std::collections::HashSet<(String, String)>,
    /// In-flight Session Rewinds, keyed like [`Self::fork_pending`].
    rewind_pending: std::collections::HashSet<(String, String)>,
    /// The `(chat id, anchor message id)` whose rewind affordance is ARMED:
    /// restarting deletes messages for good, so the first click only arms the
    /// button (danger tint + a tooltip naming the damage) and the second one
    /// inside [`REWIND_ARM_MS`] performs it.
    rewind_armed: Option<(String, String)>,
    /// Disarms [`Self::rewind_armed`] after the window elapses.
    rewind_disarm: Option<Task<()>>,
    /// In-chat find (⌘F). `None` while the find bar is closed — the shell
    /// renders the bar from this, so the two can never disagree about
    /// whether find is open.
    find: Option<FindState>,
    _style_observe: Subscription,
    _observe: Subscription,
}

/// The transcript's find index: how many matches each ROW holds, and which
/// match is current.
///
/// Rows are the granularity because rows are what the list can scroll to, and
/// because a row's match count is memoizable against the content version the
/// row diff already maintains — a streaming commit rescans only the rows
/// whose version moved, never the whole transcript. Within a row, the painter
/// resolves exact byte ranges itself (see [`crate::markdown::find`]).
#[derive(Default)]
struct FindState {
    query: String,
    /// Matches per row, parallel to [`Transcript::rows`].
    counts: Vec<u32>,
    /// Matches before each row; `prefix[i]` for row `i`, `prefix[len]` is the
    /// total. Turns a global match index into `(row, ordinal)` by search.
    prefix: Vec<u32>,
    /// Global index of the active match. Meaningless when the total is 0.
    active: usize,
    /// `row id → (row version, match count)`. Dropped whenever the query
    /// changes; otherwise it is what keeps a re-index O(changed rows).
    memo: HashMap<SharedString, (u64, u32)>,
}

impl FindState {
    fn total(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0) as usize
    }

    /// `(row index, ordinal within that row)` of the active match.
    fn target(&self) -> Option<(usize, usize)> {
        if self.total() == 0 {
            return None;
        }
        let active = self.active.min(self.total() - 1) as u32;
        // The last row whose running total is still at or below `active` —
        // rows with no matches share their neighbour's prefix, so the search
        // lands past them, on the row that actually holds the hit.
        let row = self.prefix.partition_point(|&before| before <= active) - 1;
        Some((row, (active - self.prefix[row]) as usize))
    }
}

/// One sidecar blob fetch's lifecycle.
enum BlobFetch {
    Loading(#[allow(dead_code)] Task<()>),
    /// Failed with the affordance re-armed as a retry.
    Failed,
    Ready(Arc<ToolDetail>),
}

impl Transcript {
    pub fn new(
        state: Entity<AppState>,
        comment_popup: gpui::WeakEntity<crate::comments::CommentPopup>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_options(
            state,
            comment_popup,
            crate::markdown::selection::next_transcript_scope(),
            true,
            false,
            cx,
        )
    }

    /// A temporary Side Chat transcript: the EXISTING
    /// renderer against a Side Chat fork state, with a fresh selection scope
    /// (never colliding with the main transcript or another panel), the rail
    /// disabled, narrow-panel gutters, and NO annotation actions — selection
    /// and copy stay active, but there is no Comment pill and no nested Side
    /// Chat action.
    pub fn for_side_chat(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self::with_options(
            state,
            gpui::WeakEntity::new_invalid(),
            crate::markdown::selection::next_side_chat_scope(),
            false,
            true,
            cx,
        )
    }

    fn with_options(
        state: Entity<AppState>,
        comment_popup: gpui::WeakEntity<crate::comments::CommentPopup>,
        scope: crate::markdown::selection::SelectionScope,
        rail_enabled: bool,
        embedded: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        // FollowMode stays Normal: the tail pin is ours (a per-frame spring),
        // not the list's per-layout hard snap.
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW_PX));
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event: &ListScrollEvent, _window, cx| {
            weak.update(cx, |this: &mut Transcript, cx| {
                this.handle_scroll(event, cx)
            })
            .ok();
        });
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
        let style_observe =
            cx.observe_global::<crate::chat_style::ChatAppearanceState>(|this: &mut Self, cx| {
                this.dismiss_comment_ui_and_selection(cx);
                this.render_cache.borrow_mut().clear();
                // A new tool call cap re-caps work runs, which are capped at
                // build time; the revision gate skips every other change.
                this.sync(cx);
                // Invalidate measurements without resetting the scroll anchor,
                // transcript rows, folds, or streamed content.
                this.list.remeasure_items(0..this.rows.len());
                this.spring.reset();
                this.spring_kick = this.pinned;
                this.own_turn_kick = this.own_turn.is_some();
                cx.notify();
            });
        let mut this = Self {
            state,
            list,
            rows: Vec::new(),
            chat_id: None,
            row_cache: HashMap::new(),
            live_parsers: HashMap::new(),
            tree_cache: HashMap::new(),
            folds: HashMap::new(),
            tool_details: HashMap::new(),
            tool_overflow: std::collections::HashSet::new(),
            toggle_pins: HashMap::new(),
            veils: HashMap::new(),
            veil_baseline: std::collections::HashSet::new(),
            veil_attach_pending: true,
            render_cache: Rc::new(RefCell::new(RenderCache::default())),
            highlights: HighlightStore::default(),
            show_jump_button: false,
            wheel_dy: Rc::default(),
            pinned: true,
            own_turn: None,
            own_turn_kick: false,
            own_turn_scheduled: false,
            own_turn_last_tick: None,
            spring: StickSpring::new(),
            spring_last_tick: None,
            spring_settled_at: None,
            spring_kick: false,
            spring_scheduled: false,
            scroll_anim: None,
            focus: cx.focus_handle(),
            prompt_nav: None,
            rail_enabled,
            scope,
            embedded,
            bottom_clearance: 0.0,
            viewport_size: None,
            synced_revision: None,
            rail_hover: None,
            hovered_entry: None,
            copied_code: None,
            copied_clear: None,
            copied_message: None,
            copied_message_clear: None,
            attachment_preview: None,
            attachment_preview_focus: cx.focus_handle(),
            attachment_loads: HashMap::new(),
            attachment_retries: HashMap::new(),
            blob_details: HashMap::new(),
            blob_fetch_order: HashMap::new(),
            blob_fetch_counter: 0,
            comment_popup,
            fork_pending: std::collections::HashSet::new(),
            rewind_pending: std::collections::HashSet::new(),
            rewind_armed: None,
            rewind_disarm: None,
            find: None,
            _style_observe: style_observe,
            _observe: observe,
        };
        this.sync(cx);
        this
    }
}

#[cfg(test)]
mod scroll_tests;

#[cfg(test)]
mod tests;
