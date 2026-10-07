//! The composer: a hand-rolled multiline text input (adapted from gpui's
//! `examples/input.rs`), the compact↔expanded flip, the Send/Steer/Stop morph,
//! optimistic send with failure recovery, per-chat drafts, and the question
//! wizard that replaces the composer while a run awaits input.
//!
//! Pure decision logic (flip, auto-grow math, button morph, wizard reducer,
//! pending-input detection) lives in free functions/structs with unit tests;
//! the gpui element only feeds them measurements.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AnyTooltip, App, BackgroundExecutor, BorderStyle, Bounds, ClipboardEntry,
    ClipboardItem, Context, CursorStyle, DispatchPhase, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    PaintQuad, PathPromptOptions, Pixels, Point, ScrollHandle, ScrollWheelEvent, SharedString,
    Style, StyledImage as _, Subscription, Task, TextRun, TextStyle, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, actions, div, fill, img, point, prelude::*, px, quad,
    relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use cypher_doc::{
    MessagePart, MessageRole, SessionCommandPayload, SessionMessageEntry, TranscriptFrame,
};
use cypher_engine::pi_session_modes::PiSessionModes;
use cypher_proto::{
    Chat, FileSearchMatch, HarnessId, ReasoningLevel, RunRequest, SandboxLevel, SlashCommand,
    UserInputAnswer, UserInputQuestion,
};
use cypher_rpc::{RpcError, methods};

use crate::attachments::{self, StagedAttachment};
use crate::motion;
use crate::pickers::Pickers;
use crate::state::{AppState, EngineHandle, Indicator};
use crate::theme::{MonoStyled, Theme};
mod layout;
pub use layout::*;
mod wizard;
pub use wizard::*;
mod input;
pub use input::*;
mod comments;
mod render;
mod send;
mod slash;
mod staging;

// ---------------------------------------------------------------------------
// Composer wrapper
// ---------------------------------------------------------------------------

/// Events the shell listens for.
#[derive(Debug, Clone)]
pub enum ComposerEvent {
    OpenAgentSettings {
        target_device: String,
    },
    /// Settings → GitHub for the device that answers this project's `#`
    /// lookups.
    OpenGithubSettings {
        target_device: String,
    },
    OpenProviders {
        intent: crate::settings::providers::ProviderIntent,
        target_device: Option<String>,
    },
    /// A prompt was sent optimistically — give the transcript its exact row
    /// identity so it can anchor the prompt at the top with the reply's
    /// reserved space below it.
    Sent {
        chat_id: String,
        message_id: String,
    },
}

/// Where this Composer's turns go. [`ComposerTransport::Main`] is the normal
/// chat surface (the engine's `QueueCommand` doc-command path, unchanged);
/// [`ComposerTransport::SideChat`] reroutes ONLY the RPC transport to the
/// engine's private side-chat methods (`SendSideChat` / `InterruptSideChat` /
/// `RespondSideChatInput`) while reusing the entire render path and UX.
#[derive(Debug, Clone)]
pub enum ComposerTransport {
    Main,
    SideChat(ComposerSideChat),
}

/// Whether a send consumes the composer's draft (text, staged attachments,
/// comments) or leaves it untouched — a one-click command such as the context
/// ring's `/compact` must not eat what the user is typing.
#[derive(Clone, Copy)]
enum SendDraft {
    Take,
    Keep,
}

/// The side-chat RPC identity: the temporary chat's id and the authoritative
/// host device (`targetDeviceId` rides every side-chat RPC when it differs
/// from the connected engine's).
#[derive(Debug, Clone)]
pub struct ComposerSideChat {
    pub side_chat_id: String,
    pub target_device_id: String,
}

impl ComposerSideChat {
    /// The RunRequest for a side-chat turn: inherited harness/model/reasoning/
    /// options from the fork's synthetic row (the parent's config), the
    /// inherited cwd, and the inherited sandbox. Pure — unit-tested.
    // One argument per `RunRequest` field: a params struct here would just be
    // `RunRequest` again, one construction earlier.
    #[allow(clippy::too_many_arguments)]
    pub fn run_request(
        prompt: String,
        cwd: String,
        harness: Option<HarnessId>,
        model: Option<String>,
        reasoning: Option<ReasoningLevel>,
        model_options: serde_json::Map<String, serde_json::Value>,
        sandbox: SandboxLevel,
        attachments: Vec<String>,
    ) -> RunRequest {
        RunRequest {
            prompt,
            harness,
            model,
            reasoning,
            model_options,
            cwd,
            sandbox,
            auto_approve: false,
            resume: None,
            attachments,
            pending_attachments: Vec::new(),
            worktree: None,
        }
    }

    /// Merge `targetDeviceId` into the params when the side chat's host device
    /// differs from the connected engine's own.
    pub fn with_target(
        &self,
        params: &mut serde_json::Map<String, serde_json::Value>,
        local_device_id: Option<&str>,
    ) {
        if local_device_id.is_some_and(|local| local != self.target_device_id.as_str()) {
            params.insert(
                "targetDeviceId".into(),
                serde_json::Value::String(self.target_device_id.clone()),
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionToken {
    range: Range<usize>,
    query: String,
}

/// The `@` must begin a token. This intentionally excludes `name@example.com`
/// and ordinary words while allowing punctuation such as `(@src`.
fn mention_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let token_start = text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at + ch.len_utf8()))
        .unwrap_or(0);
    let relative_at = text[token_start..cursor].rfind('@')?;
    let at = token_start + relative_at;
    let valid_boundary = at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{'));
    if text[at + 1..cursor].contains('@') || !valid_boundary {
        return None;
    }
    let end = text[cursor..]
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(cursor + at))
        .unwrap_or(text.len());
    Some(MentionToken {
        range: at..end,
        query: text[at + 1..cursor].to_string(),
    })
}

/// The `#` must begin a token, like `@`: `C#`, `a#b`, and a Markdown
/// heading's `##` never open the issue popup, and the query stops at the
/// next `#`/`@` so the two completions can't overlap.
fn issue_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let token_start = text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at + ch.len_utf8()))
        .unwrap_or(0);
    let relative_hash = text[token_start..cursor].rfind('#')?;
    let hash = token_start + relative_hash;
    let valid_boundary = hash == 0
        || text[..hash]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{'));
    let end = text[cursor..]
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(cursor + at))
        .unwrap_or(text.len());
    let rest = &text[hash + 1..end];
    if !valid_boundary || rest.contains(['#', '@']) {
        return None;
    }
    Some(MentionToken {
        range: hash..end,
        query: text[hash + 1..cursor].to_string(),
    })
}

/// The `/` must open the input: slash commands are whole-prompt prefixes
/// (`/compact`, `/goal ship it`), so only the first token triggers, and a
/// query containing another `/` (a typed path) never does.
fn slash_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) || !text.starts_with('/') {
        return None;
    }
    let end = text
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at))
        .unwrap_or(text.len());
    // Cursor outside the command token (typing the argument): popup closed.
    if cursor == 0 || cursor > end {
        return None;
    }
    let query = &text[1..cursor];
    if query.contains('/') {
        return None;
    }
    Some(MentionToken {
        range: 0..end,
        query: query.to_string(),
    })
}

/// A `/` menu row's state badge ([`crate::slash_menu::command_badge`]):
/// green when something is on or running, amber for a reading worth acting
/// on, quiet otherwise.
fn slash_badge(theme: &Theme, badge: crate::slash_menu::Badge) -> gpui::Div {
    use crate::slash_menu::Tone;
    let (background, color) = match badge.tone {
        Tone::On => (theme.success.opacity(0.14), theme.success),
        Tone::Warning => (theme.warning.opacity(0.16), theme.warning),
        Tone::Neutral => (crate::theme::ink(0.06), theme.text_muted),
        Tone::Off => (crate::theme::ink(0.04), theme.text_muted.opacity(0.75)),
    };
    div()
        .flex_none()
        .h(px(18.0))
        // "On" and "Off" share a width, so flipping one does not shift it.
        .min_w(px(30.0))
        .px(px(6.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(background)
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color)
        .child(SharedString::from(badge.label))
}

/// The leading `/command` token of a whole-prompt slash send, if any.
/// Used to restyle settings-style extension menus (not agent questions).
pub fn slash_command_label(text: &str) -> Option<&str> {
    let t = text.trim();
    if !t.starts_with('/') || t.starts_with("//") {
        return None;
    }
    let cmd = t.split_whitespace().next().unwrap_or(t);
    (cmd.len() >= 2).then_some(cmd)
}

/// Slash-command completion state: like [`MentionState`] but the
/// candidate list is fetched once per harness (`ListCommands`) and filtered
/// locally per keystroke — no RPC, debounce, or skeleton churn while typing.
#[derive(Debug, Clone, Default)]
struct SlashState {
    token: Option<MentionToken>,
    /// The command whose choices are open (`/orchestrate o…`), or `None` for
    /// the command list.
    parent: Option<String>,
    /// The rows on show ([`crate::slash_menu`]).
    menu: crate::slash_menu::Menu,
    /// The highlighted row, as a position in `menu.selectable`.
    active: Option<usize>,
    /// Harness the popup is showing commands for (cache key).
    harness: Option<HarnessId>,
    request: u64,
    loading: bool,
    error: Option<SharedString>,
    dismissed: Option<(Range<usize>, String)>,
}

/// `#` issue and pull request completion: rows come from `SearchGithubIssues`
/// on the project's host device (debounced like file search; stale rows stay
/// visible while a refinement is in flight).
#[derive(Debug, Clone, Default)]
struct IssueState {
    token: Option<MentionToken>,
    /// `owner/name` the rows belong to.
    repo: Option<String>,
    issues: Vec<cypher_proto::GithubIssueSummary>,
    pull_requests: Vec<cypher_proto::GithubIssueSummary>,
    /// Index into the issue rows followed by the pull request rows.
    active: Option<usize>,
    request: u64,
    loading: bool,
    /// Why there are no rows to pick: no GitHub login on the device, no
    /// access to the repository, no GitHub remote, or a failed search.
    notice: Option<SharedString>,
    /// The fix the notice offers, as a clickable row under it.
    action: Option<IssueAction>,
    dismissed: Option<(Range<usize>, String)>,
}

impl IssueState {
    fn row_count(&self) -> usize {
        self.issues.len() + self.pull_requests.len()
    }

    /// Row `ix` of the popup, and whether it is a pull request.
    fn row(&self, ix: usize) -> Option<(&cypher_proto::GithubIssueSummary, bool)> {
        match ix.checked_sub(self.issues.len()) {
            None => self.issues.get(ix).map(|row| (row, false)),
            Some(pull) => self.pull_requests.get(pull).map(|row| (row, true)),
        }
    }

    /// Row `ix`'s child index in the scroll list, where each non-empty
    /// section leads with a header row.
    fn scroll_index(&self, ix: usize) -> usize {
        ix + 1 + usize::from(ix >= self.issues.len() && !self.issues.is_empty())
    }

    fn clear_rows(&mut self) {
        self.issues.clear();
        self.pull_requests.clear();
        self.active = None;
    }
}

/// What the `#` popup's notice row does when clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IssueAction {
    /// Open Settings → GitHub for this device.
    SignIn { device: String },
    /// Install the Cypher GitHub App on the repository.
    Install { url: String, repo: Option<String> },
}

/// Where `#` lookups run: the current chat's checkout, or the new-chat
/// canvas's project (and picked worktree), on that project's host device.
struct IssueScope {
    params: serde_json::Map<String, serde_json::Value>,
    target: String,
    space: String,
}

/// The popup line for a project that can't list issues.
fn issue_unavailable_message(
    reason: cypher_proto::GithubUnavailable,
    repo: Option<&str>,
    device: &str,
) -> String {
    match reason {
        cypher_proto::GithubUnavailable::SignedOut => {
            format!("Sign in to GitHub on {device} to reference issues")
        }
        cypher_proto::GithubUnavailable::NoAccess => format!(
            "The GitHub login on {device} can't see {}",
            repo.unwrap_or("this repository")
        ),
        cypher_proto::GithubUnavailable::NoGithubRemote => {
            "This project has no GitHub remote".to_string()
        }
    }
}

fn issue_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The project's device runs an older Cypher — update it to reference issues".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The project's device is unreachable".into(),
        RpcError::BadParams(_) => "Issue search failed".into(),
        RpcError::Failed(message) => format!("Issue search failed: {message}").into(),
    }
}

/// One `@session` popup candidate: a durable chat (see [`MentionSession`]).
#[derive(Debug, Clone)]
struct MentionSession {
    chat_id: String,
    device_id: String,
    title: String,
    archived: bool,
    /// The chat's space id, for the popup's project/device subtitle.
    project: Option<String>,
}

/// What an accepted mention row inserts.
#[derive(Debug, Clone)]
enum MentionCandidate {
    Session(MentionSession),
    File(FileSearchMatch),
}

/// The combined mention popup state: durable session candidates (filtered
/// locally and deterministically from synced chats) above the file results
/// (the existing `SearchFiles` RPC, debounced). ONE keyboard active index
/// walks the flattened [sessions, files] list.
#[derive(Debug, Clone, Default)]
struct MentionState {
    token: Option<MentionToken>,
    sessions: Vec<MentionSession>,
    files: Vec<FileSearchMatch>,
    active: Option<usize>,
    request: u64,
    /// File search in flight.
    loading: bool,
    /// Why the last file search failed, for the popup. A failure MUST NOT
    /// render as "No matching files": cross-device searches fail for reasons
    /// the user can act on (host daemon too old for `SearchFiles`, device
    /// offline), and the empty state hid them (user report).
    error: Option<SharedString>,
    /// Full token text, not just the cursor-relative query: moving within a
    /// dismissed token keeps it closed, while any edit re-enables completion.
    dismissed: Option<(Range<usize>, String)>,
}

impl MentionState {
    /// Total candidate rows in the flattened [sessions, files] list.
    fn count(&self) -> usize {
        self.sessions.len() + self.files.len()
    }
}

fn mention_response_is_current(state: &MentionState, request: u64) -> bool {
    state.request == request && state.token.is_some()
}

/// The display title for a durable chat: the synced title, else a sensible
/// preview, else the placeholder. Bounded so chips and popup rows stay compact.
fn session_display_title(chat: &Chat) -> String {
    let raw = chat
        .title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            chat.last_message_preview
                .as_deref()
                .filter(|t| !t.trim().is_empty())
        })
        .unwrap_or("Untitled session");
    let trimmed = raw.trim();
    if trimmed.chars().count() > MAX_SESSION_TITLE_CHARS {
        let mut out: String = trimmed.chars().take(MAX_SESSION_TITLE_CHARS).collect();
        out.push('…');
        out
    } else {
        trimmed.to_string()
    }
}

/// How close a candidate session is to the chat being composed: the current
/// project (canonical `space_id`, Sidebar grouping's exact identity) first,
/// then another project on the same host device, then any other device.
fn session_proximity(chat: &Chat, project: Option<&str>, device: Option<&str>) -> u8 {
    if chat.space_id.as_deref() == project {
        0
    } else if device == Some(chat.device_id.as_str()) {
        1
    } else {
        2
    }
}

/// Local, deterministic `@session` candidates from synced chats: durable root
/// sessions from ANY project and ANY device of the active profile, never
/// children or the current chat. Non-archived sessions show for any query;
/// archived sessions only appear once the query is non-empty. Ranking is
/// stable: non-archived first, then proximity (current project, same device,
/// other devices — see [`session_proximity`]), then most recently active,
/// then title, then id. At most [`MAX_SESSION_CANDIDATES`] rows are returned
/// so the file results stay visible under them.
fn session_candidates(
    chats: &[Chat],
    query: &str,
    current_chat: Option<&str>,
    project: Option<&str>,
    device: Option<&str>,
) -> Vec<MentionSession> {
    let query = query.trim().to_lowercase();
    let mut candidates: Vec<(&Chat, String)> = Vec::new();
    for chat in chats {
        if chat.is_child() || current_chat == Some(chat.id.as_str()) {
            continue;
        }
        if chat.archived && query.is_empty() {
            continue;
        }
        let title = session_display_title(chat);
        if !query.is_empty()
            && !title.to_lowercase().contains(&query)
            && !chat.id.to_lowercase().contains(&query)
        {
            continue;
        }
        candidates.push((chat, title));
    }
    candidates.sort_by(|(a, title_a), (b, title_b)| {
        a.archived
            .cmp(&b.archived)
            .then_with(|| {
                session_proximity(a, project, device).cmp(&session_proximity(b, project, device))
            })
            .then_with(|| b.last_message_at.cmp(&a.last_message_at))
            .then_with(|| title_a.to_lowercase().cmp(&title_b.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    candidates
        .into_iter()
        .take(MAX_SESSION_CANDIDATES)
        .map(|(chat, title)| MentionSession {
            chat_id: chat.id.clone(),
            device_id: chat.device_id.clone(),
            title,
            archived: chat.archived,
            project: chat.space_id.clone(),
        })
        .collect()
}

/// A failed file search, translated for the popup. `UnknownMethod` is the
/// version-skew case: `SearchFiles` shipped after v0.1.9, so a session hosted
/// by a device on an older daemon answers "unknown method" while the same
/// search works for local sessions.
fn mention_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older Cypher — update it to search its files".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => "File search failed".into(),
    }
}

/// A failed command discovery, translated for the popup.
fn slash_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older Cypher — update it to list commands".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => {
            "Couldn't load this agent's commands".into()
        }
    }
}

pub struct Composer {
    /// A style change needs fresh child measurements, then one pass after a
    /// possible compact/expanded flip. Never depend on typing to finish sizing.
    style_relayout_passes: u8,
    state: Entity<AppState>,
    input: Entity<ComposerInput>,
    /// Composer actions row: repo/branch/harness-model/traits.
    /// Shared with the shell's new-session canvas, which renders the
    /// device/project target selectors ([`Pickers::render_target_selectors`]).
    pickers: Entity<Pickers>,
    /// Draft text per chat key ("" = new-chat canvas), surviving navigation.
    drafts: HashMap<String, String>,
    /// Staged-but-unsent attachments per chat key (use-attachments.ts `stash`):
    /// navigating away and back restores them; memory-only, like the original.
    attachments: HashMap<String, Vec<StagedAttachment>>,
    /// The staged attachment being viewed full-size (click a thumbnail).
    preview: Option<attachments::PreviewImage>,
    /// Focused while the lightbox is open so Escape reaches it; the input
    /// gets focus back on close.
    preview_focus: FocusHandle,
    /// Focus grab deferred to the next render, so an open site without a
    /// `Window` can still hand focus to the lightbox.
    preview_focus_pending: bool,
    /// In-flight file-picker prompt (paperclip).
    picker_task: Option<Task<()>>,
    mention_task: Option<Task<()>>,
    mention: MentionState,
    slash_task: Option<Task<()>>,
    /// Background ListCommands so the first `/` is not a cold Pi spawn.
    slash_prefetch: Option<Task<()>>,
    slash: SlashState,
    issue_task: Option<Task<()>>,
    issue: IssueState,
    /// Scroll position of the `#` popup's list.
    issue_scroll: ScrollHandle,
    /// Projects already known to have no GitHub remote: `#` stays plain text
    /// there instead of reopening the notice on every heading or hashtag.
    no_github_spaces: HashSet<String>,
    /// Scroll position of the `/` popup's command list (rows are the scroll
    /// container's direct children, so keyboard `scroll_to_item(active)`
    /// maps 1:1 — the pickers' model-menu pattern).
    slash_scroll: ScrollHandle,
    /// Scroll position of the `@` popup's list (see [`mention_scroll_child`]).
    mention_scroll: ScrollHandle,
    /// Advertised commands per harness. Refetched when Settings → Agents
    /// toggles a Pi package (`HarnessCatalogChanged`). Settings → Commands
    /// hide/show filters this list in [`Self::refilter_slash`].
    slash_cache: HashMap<HarnessId, Vec<SlashCommand>>,
    slash_owner: Option<String>,
    slash_generation: u64,
    /// The Pi plugins' switches for a chat (`None` = no chat), refetched each
    /// time the `/` menu opens and kept between openings so its badges do
    /// not flicker.
    slash_modes: Option<(Option<String>, PiSessionModes)>,
    slash_modes_task: Option<Task<()>>,
    slash_modes_request: u64,
    current_key: String,
    sending: bool,
    failure: Option<SharedString>,
    wizard: Option<Wizard>,
    wizard_focus: FocusHandle,
    /// Selection scope of the question card's text (question, context and
    /// option copy select + copy like transcript text).
    wizard_selection: crate::markdown::selection::SelectionScope,
    /// Requests already answered locally (suppresses the panel until the doc
    /// frame marks them resolved).
    answered_requests: HashSet<String>,
    advance_task: Option<Task<()>>,
    /// When the last answer went out. A card mounting within
    /// [`WIZARD_HANDOFF_QUIET_MS`] of it is the follow-up stage, and swaps in
    /// without an entrance fade.
    answered_at: Option<Instant>,
    /// The composer came back from an answer, so it too swaps in with no fade.
    /// Only ever flipped while the composer is UNMOUNTED (a question panel is
    /// up), because `with_animation` replays from zero the moment its element
    /// reappears — toggling this under a live composer would flash it.
    input_swap_instant: bool,
    send_task: Option<Task<()>>,
    /// Where turns go: the normal chat surface, or a temporary Side Chat's
    /// private RPC transport (same render path, branched transport only).
    transport: ComposerTransport,
    /// Ordered pending comments for the CURRENT chat. Cleared on
    /// chat switch; snapshotted (then optimistically hidden) on send, restored
    /// on queue failure, gone on acceptance.
    comments: Vec<DraftComment>,
    /// Open/close lifecycle for the comments inspector popover.
    comments_popup: crate::popover::Popup<()>,
    /// A comment row being edited inside the inspector.
    comment_edit: Option<CommentEdit>,
    // -- compact/expanded flip state (hysteresis; see `composer_flip`) --
    /// Current layout mode (persisted across frames — never derived fresh).
    expanded_mode: bool,
    /// `layout_epoch` of the measurement that caused the last flip: the flip is
    /// re-evaluated only after the input has been laid out in the new mode, so
    /// at most one flip can happen per layout pass.
    flip_epoch: u64,
    /// Compact-mode input capacity, learned while compact (layout-stable).
    compact_capacity: f32,
    /// Input width first measured after expanding — container-width deltas
    /// while expanded shift `compact_capacity` by the same amount.
    expanded_anchor: f32,
    /// Last input width seen in the current mode (resize detection).
    last_seen_width: f32,
    /// Set while an interactive resize is in flight; mode is frozen until
    /// widths have settled for [`RESIZE_SETTLE_MS`].
    width_changed_at: Option<Instant>,
    settle_task: Option<Task<()>>,
    /// In-flight compact↔expanded morph (one per committed flip; manual
    /// drive — see [`FlipMorph`]).
    flip_morph: Option<FlipMorph>,
    /// Pill height actually rendered last frame — a committed flip morphs
    /// from here, so mid-flight reversals hand off without a jump.
    last_rendered_height: f32,
    /// Monotonic clock anchor for the morph timeline.
    morph_clock: Instant,
    /// The pill's laid-out width (last frame), for the narrow-tile gate.
    pill_width: Rc<std::cell::Cell<f32>>,
    /// Set on every session/route change: flips committed before this instant
    /// SNAP instead of morphing (see [`ROUTE_SNAP_MS`]).
    route_snap_until: Option<Instant>,
    _observe: Subscription,
    _pickers_observe: Subscription,
    _picker_events: Subscription,
    _catalog_observe: Subscription,
    _shown_slash_observe: Subscription,
    _style_observe: Subscription,
    _input_events: Subscription,
}

/// A comment row being edited inside the inspector: its index into
/// [`Composer::comments`], the input entity holding the draft, and the input
/// subscription (Enter saves).
struct CommentEdit {
    index: usize,
    input: Entity<ComposerInput>,
    _events: Subscription,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn report_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.failure = Some(message.into());
        cx.notify();
    }
    /// The picker entity, for the shell's canvas target selectors.
    pub fn pickers(&self) -> &Entity<Pickers> {
        &self.pickers
    }

    /// Target the new-session canvas at an existing checkout of `space_id`
    /// (the sidebar's hover add buttons). Delegates to [`Pickers::target_checkout`],
    /// which pins the plan space-scoped so it's authoritative without the refs
    /// list ever loading.
    pub fn target_checkout(
        &mut self,
        space_id: String,
        plan: crate::pickers::CheckoutPlan,
        cx: &mut Context<Self>,
    ) {
        self.pickers.update(cx, |pickers, cx| {
            pickers.target_checkout(space_id, plan, cx);
        });
    }

    /// Drop any programmatic checkout target (the sidebar's hover add buttons)
    /// so the canvas reads generically again. The global "new session" action
    /// calls this before routing to the canvas. Delegates to
    /// [`Pickers::clear_checkout_target`].
    pub fn clear_checkout_target(&mut self, cx: &mut Context<Self>) {
        self.pickers.update(cx, |pickers, cx| {
            pickers.clear_checkout_target();
            cx.notify();
        });
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self::with_transport(state, ComposerTransport::Main, cx)
    }

    /// A Composer bound to a Side Chat fork ([`AppState::new_side_chat_fork`]):
    /// the SAME component and render path as the main surface, with the RPC
    /// transport branched to the engine's private side-chat methods. The fork
    /// state's synthetic selected row makes every inherited config read
    /// (`selected_chat_row`, `resolved`) resolve to the parent's working
    /// context; the model/traits pickers start there and stamp picks locally.
    pub fn for_side_chat(
        state: Entity<AppState>,
        side_chat: ComposerSideChat,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_transport(state, ComposerTransport::SideChat(side_chat), cx)
    }

    fn with_transport(
        state: Entity<AppState>,
        transport: ComposerTransport,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = ComposerInput::new("Do anything…", cx);
            input.use_chat_style = true;
            input.line_height = px(crate::chat_style::settings(cx).input_line_height());
            input.content_height = f32::from(input.line_height);
            input.enable_mentions();
            input
        });
        let pickers = cx.new(|cx| {
            let mut pickers = Pickers::new(state.clone(), cx);
            // A temporary side chat's model/traits chips start on the
            // inherited values; picks stay on the fork's synthetic row (never
            // a `setChatConfig` target) and ride every side-chat send.
            if matches!(transport, ComposerTransport::SideChat(_)) {
                pickers.set_side_chat();
            }
            pickers
        });
        // The footer toolbar (checkout kind + ref picker) is rendered INLINE
        // by the composer from picker state — a pickers-side notify (refs
        // loaded, popover toggled, pick made) must repaint the composer too.
        let pickers_observe = cx.observe(&pickers, |_, _, cx| cx.notify());
        let picker_events = cx.subscribe(&pickers, |_: &mut Self, _, event, cx| match event {
            crate::pickers::PickerEvent::OpenAgentSettings { target_device } => {
                cx.emit(ComposerEvent::OpenAgentSettings {
                    target_device: target_device.clone(),
                });
            }
        });
        let shown_slash_observe = cx
            .observe_global::<crate::settings::commands::ShownSlashCommands>(
                |this: &mut Self, cx| {
                    // Re-open an open menu from scratch: the first command
                    // turned on needs the agent's list fetched, which a menu
                    // with only the actions skipped.
                    let (text, cursor) = {
                        let input = this.input.read(cx);
                        (input.text().to_string(), input.cursor_offset())
                    };
                    this.slash.token = None;
                    this.slash.parent = None;
                    this.update_slash(&text, cursor, cx);
                    this.prefetch_slash_commands(cx);
                    cx.notify();
                },
            );
        let catalog_observe =
            cx.observe_global::<crate::pickers::HarnessCatalogChanged>(|this: &mut Self, cx| {
                this.slash_cache.clear();
                this.slash_generation = this.slash_generation.wrapping_add(1);
                this.slash_prefetch = None;
                this.slash_task = None;
                this.slash.request = this.slash.request.wrapping_add(1);
                if this.slash.token.is_some() {
                    this.slash.harness = None;
                    let (text, cursor) = {
                        let input = this.input.read(cx);
                        (input.text().to_string(), input.cursor_offset())
                    };
                    this.update_slash(&text, cursor, cx);
                } else {
                    this.prefetch_slash_commands(cx);
                }
                cx.notify();
            });
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.on_state_changed(cx));
        let style_observe =
            cx.observe_global::<crate::chat_style::ChatAppearanceState>(|this: &mut Self, cx| {
                this.flip_morph = None;
                this.style_relayout_passes = 2;
                this.flip_epoch = this.input.read(cx).layout_epoch;
                this.last_seen_width = 0.0;
                this.width_changed_at = None;
                this.compact_capacity = 0.0;
                this.expanded_anchor = 0.0;
                this.input.update(cx, |_, cx| cx.notify());
                cx.notify();
            });
        let input_events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.on_submit(cx),
            ComposerInputEvent::Edited | ComposerInputEvent::CursorMoved => {
                this.on_input_edited(cx)
            }
            ComposerInputEvent::ViewportChanged => cx.notify(),
            // The slash popup and the mention popup share the input's
            // completion key routing; they are mutually exclusive by token
            // shape (`/` at offset 0 vs `@` at a token boundary).
            ComposerInputEvent::MentionNavigate(delta) => {
                if this.slash.token.is_some() {
                    this.move_slash(*delta, cx)
                } else if this.issue.token.is_some() {
                    this.move_issue(*delta, cx)
                } else {
                    this.move_mention(*delta, cx)
                }
            }
            ComposerInputEvent::MentionAccept => {
                if this.slash.token.is_some() {
                    this.accept_slash(cx)
                } else if this.issue.token.is_some() {
                    this.accept_issue(cx)
                } else {
                    this.accept_mention(cx)
                }
            }
            ComposerInputEvent::MentionDismiss => {
                if this.slash.token.is_some() {
                    this.dismiss_slash(cx)
                } else if this.issue.token.is_some() {
                    this.dismiss_issue(cx)
                } else {
                    this.dismiss_mention(cx)
                }
            }
            ComposerInputEvent::PastedImages(images) => {
                let staged = images
                    .iter()
                    .map(|image| attachments::stage_clipboard_image(image.clone()))
                    .collect();
                this.add_staged(staged, cx);
            }
            ComposerInputEvent::PastedPaths(paths) => this.add_paths(paths.clone(), cx),
        });
        let current_key = state.read(cx).selected_chat.clone().unwrap_or_default();
        let mut composer = Self {
            style_relayout_passes: 2,
            state,
            input,
            pickers,
            drafts: HashMap::new(),
            attachments: HashMap::new(),
            preview: None,
            preview_focus: cx.focus_handle(),
            preview_focus_pending: false,
            picker_task: None,
            mention_task: None,
            mention: MentionState::default(),
            slash_task: None,
            slash_prefetch: None,
            slash: SlashState::default(),
            issue_task: None,
            issue: IssueState::default(),
            issue_scroll: ScrollHandle::new(),
            no_github_spaces: HashSet::new(),
            slash_scroll: ScrollHandle::new(),
            mention_scroll: ScrollHandle::new(),
            slash_cache: HashMap::new(),
            slash_owner: None,
            slash_modes: None,
            slash_modes_task: None,
            slash_modes_request: 0,
            slash_generation: 0,
            current_key,
            sending: false,
            failure: None,
            wizard: None,
            wizard_focus: cx.focus_handle(),
            wizard_selection: crate::markdown::selection::next_question_scope(),
            answered_requests: HashSet::new(),
            advance_task: None,
            answered_at: None,
            input_swap_instant: false,
            send_task: None,
            transport,
            comments: Vec::new(),
            comments_popup: crate::popover::Popup::default(),
            comment_edit: None,
            expanded_mode: false,
            flip_epoch: 0,
            compact_capacity: 0.0,
            expanded_anchor: 0.0,
            last_seen_width: 0.0,
            width_changed_at: None,
            settle_task: None,
            flip_morph: None,
            last_rendered_height: 0.0,
            morph_clock: Instant::now(),
            pill_width: Rc::default(),
            route_snap_until: None,
            _observe: observe,
            _pickers_observe: pickers_observe,
            _picker_events: picker_events,
            _catalog_observe: catalog_observe,
            _shown_slash_observe: shown_slash_observe,
            _style_observe: style_observe,
            _input_events: input_events,
        };
        composer.prefetch_slash_commands(cx);
        composer
    }

    /// Capture-knob passthrough (`CYPHER_OPEN_DIALOG=model`): open the
    /// combined harness/model menu.
    pub fn debug_open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pickers
            .update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
    }

    pub fn is_sending(&self) -> bool {
        self.sending
    }

    #[cfg(test)]
    pub(crate) fn set_sending_for_test(&mut self, sending: bool) {
        self.sending = sending;
    }
}

#[cfg(test)]
mod tests;
