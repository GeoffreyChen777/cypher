//! Session-level Subagents: the live status of the CURRENT chat's subagent
//! runs, aggregated from the transcript's subagent tool parts and the
//! engine's live `Session.subagents` snapshot (`cypher.subagents.v1`).
//!
//! Deliberately NOT a transcript row (no `RowKind`/cache entry): it is chrome
//! on the right of the status strip — a compact trigger that opens an
//! UPWARD inspector popover for the current chat — and pure over what the
//! transcript + the session row already carry.
//!
//! State rules (the only way a run reads as live `Running` is a structured
//! `cypher.subagents.v1` snapshot):
//! - **snapshot** is the single authority for `Running`/`Stale` (async
//!   Running/Done/Error fully wins; sync doc terminal state still wins over a
//!   stale snapshot). A snapshot that goes away never resurrects a doc-only
//!   launch ack.
//! - **sync**: the doc ToolResult is the terminal truth; the snapshot only
//!   supplements model/progress while not doc-terminal.
//! - **async**: the outer ToolResult is just a launch ACK — ignored entirely
//!   without a snapshot; a resolved async `is_error` is a durable Error
//!   fallback (the launch failed).
//! - an unresolved part only reads as `Starting` while its assistant entry is
//!   still Streaming; an unresolved part on a settled entry (Complete/Aborted)
//!   is ignored — a dead turn must never ghost a runner.
//! - `SUBAGENT_STALE_MS` is a visual guard when a snapshot's heartbeat goes
//!   quiet (never a terminal judgment).

use std::collections::HashMap;

use chrono::Utc;
use gpui::{
    AnyElement, Context, Empty, Entity, FocusHandle, KeyDownEvent, Render, SharedString,
    Subscription, Window, div, prelude::*, px,
};

use cypher_doc::{MessagePart, MessageStatus, SessionMessageEntry};
use cypher_proto::{
    SessionStatus, SubagentRun, SubagentRunMode, SubagentRunStatus, view::subagent_call_info,
};

use crate::motion;
use crate::popover::{self, Popup};
use crate::state::AppState;
use crate::theme::Theme;

/// A running/async-started entry whose freshness went quiet is stale past
/// this (matches the session staleness window).
pub const SUBAGENT_STALE_MS: i64 = 45_000;

/// Derived display status for one merged entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelStatus {
    Running,
    Done,
    Error,
    /// UI-only in-flight marker: the doc part is unresolved and its assistant
    /// entry is still Streaming, but no structured snapshot has arrived yet.
    /// NOT a public protocol status — it is purely a panel label, and it can
    /// never be produced by a snapshot (only `snapshot_status` yields
    /// Running/Stale).
    Starting,
    /// A running entry whose freshness went quiet.
    Stale,
}

/// One merged row of the inspector.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentPanelEntry {
    pub run_id: String,
    pub tool_call_id: Option<String>,
    pub agent: String,
    pub task: String,
    pub model: Option<String>,
    pub mode: SubagentRunMode,
    pub status: PanelStatus,
    pub progress: Option<String>,
    pub started_at: i64,
    pub updated_at: i64,
    pub ended_at: Option<i64>,
    /// Cypher child chat id backing this run (engine-owned): the row is
    /// clickable and navigation selects the child's real live session.
    pub child_chat_id: Option<String>,
}

impl SubagentPanelEntry {
    /// A row with a Cypher child chat behind it (navigable via the Inspector).
    pub fn is_navigable(&self) -> bool {
        self.child_chat_id.is_some()
    }
}

/// Inspector header / trigger counts. The statuses are kept SPLIT (a stale
/// runner is not a live running one, and a starting run is not running) so
/// the trigger copy and the header line stay accurate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PanelCounts {
    /// Live [`PanelStatus::Running`] entries (snapshot-produced only).
    pub running: usize,
    /// Quiet runners ([`PanelStatus::Stale`]).
    pub stale: usize,
    /// Unresolved + streaming with no snapshot yet ([`PanelStatus::Starting`])
    /// — in-flight, never Running.
    pub starting: usize,
    pub done: usize,
    pub failed: usize,
}

impl SubagentPanelEntry {
    fn in_flight(status: PanelStatus) -> bool {
        matches!(
            status,
            PanelStatus::Running | PanelStatus::Stale | PanelStatus::Starting
        )
    }
}

/// The doc-only status for a subagent tool part. Only a structured snapshot
/// can produce Running/Stale — the doc alone can only ever suggest in-flight
/// (Starting) or a durable terminal:
/// - unresolved + assistant entry still Streaming → Starting (no snapshot yet,
///   so it must never read Running).
/// - unresolved but the entry is settled (Complete/Aborted) → `None` (ignored:
///   a dead turn must not ghost a runner).
/// - resolved async success is just a launch ACK → `None` (completely ignored
///   — no snapshot, no run).
/// - resolved async `is_error` → Error (durable fallback — the launch failed).
/// - resolved sync → Done/Error from the result's is_error.
fn derive_doc_status(
    is_async: bool,
    is_error: bool,
    resolved: bool,
    streaming: bool,
) -> Option<PanelStatus> {
    if !resolved {
        return streaming.then_some(PanelStatus::Starting);
    }
    if is_async {
        return is_error.then_some(PanelStatus::Error);
    }
    Some(if is_error {
        PanelStatus::Error
    } else {
        PanelStatus::Done
    })
}

fn snapshot_status(run: &SubagentRun, now_ms: i64) -> PanelStatus {
    match run.status {
        SubagentRunStatus::Running => {
            if now_ms.saturating_sub(run.updated_at) > SUBAGENT_STALE_MS {
                PanelStatus::Stale
            } else {
                PanelStatus::Running
            }
        }
        SubagentRunStatus::Done => PanelStatus::Done,
        SubagentRunStatus::Error => PanelStatus::Error,
    }
}

/// Merge the doc-derived entry with a live snapshot run (same tool call id).
/// Async: snapshot wins (the doc result was only the launch ack). Sync: the
/// doc terminal state wins, snapshot supplements model/progress/freshness.
fn merge_snapshot(entry: &mut SubagentPanelEntry, run: &SubagentRun, now_ms: i64) {
    // The extension's uuid is the run's stable identity (the doc part id is
    // the tool call id, which is stable too — but the uuid is what the board
    // keeps, so prefer it for the panel's row key).
    entry.run_id = run.run_id.clone();
    entry.model = run.model.clone();
    entry.started_at = entry.started_at.min(run.started_at);
    entry.updated_at = entry.updated_at.max(run.updated_at);
    entry.ended_at = run.ended_at;
    entry.child_chat_id = run.child_chat_id.clone();
    if entry.progress.is_none() {
        entry.progress = run.progress.clone();
    }
    match entry.mode {
        SubagentRunMode::Async => {
            entry.status = snapshot_status(run, now_ms);
        }
        SubagentRunMode::Sync => match entry.status {
            PanelStatus::Done | PanelStatus::Error => {}
            _ => entry.status = snapshot_status(run, now_ms),
        },
        SubagentRunMode::Message => {
            entry.status = snapshot_status(run, now_ms);
        }
    }
}

fn from_snapshot(run: &SubagentRun, now_ms: i64) -> SubagentPanelEntry {
    SubagentPanelEntry {
        run_id: run.run_id.clone(),
        tool_call_id: run.tool_call_id.clone(),
        agent: run.agent.clone(),
        task: run.task.clone(),
        model: run.model.clone(),
        mode: run.mode,
        status: snapshot_status(run, now_ms),
        progress: run.progress.clone(),
        started_at: run.started_at,
        updated_at: run.updated_at,
        ended_at: run.ended_at,
        child_chat_id: run.child_chat_id.clone(),
    }
}

/// Aggregate the current chat's subagent runs: doc tool parts keyed by tool
/// call id, merged with the live snapshot; message-mode snapshot runs (no
/// tool call id) insert by run id. Pure — the trigger/inspector render this
/// directly.
pub fn aggregate_subagents(
    entries: &[SessionMessageEntry],
    snapshot: &[SubagentRun],
    now_ms: i64,
) -> Vec<SubagentPanelEntry> {
    let mut out: Vec<SubagentPanelEntry> = Vec::new();
    let mut by_tool: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        // The doc alone can only label an unresolved part while its entry is
        // actually streaming — a settled entry's unresolved part is a ghost.
        let streaming = entry.status == Some(MessageStatus::Streaming);
        for part in &entry.parts {
            let MessagePart::Tool {
                id,
                call,
                is_error,
                resolved,
                progress,
                ..
            } = part
            else {
                continue;
            };
            let Some(info) = subagent_call_info(call) else {
                continue;
            };
            let created = entry.created_at;
            let is_async = info.is_async;
            let Some(status) = derive_doc_status(is_async, *is_error, *resolved, streaming) else {
                // Doc-only parts that can never be a real run (settled
                // unresolved, or a resolved async launch ack with no snapshot)
                // are dropped — they are what used to ghost the panel.
                continue;
            };
            by_tool.insert(id.as_str(), out.len());
            out.push(SubagentPanelEntry {
                run_id: id.clone(),
                tool_call_id: Some(id.clone()),
                agent: info.agent,
                task: info.task,
                model: None,
                mode: if is_async {
                    SubagentRunMode::Async
                } else {
                    SubagentRunMode::Sync
                },
                status,
                progress: progress.clone(),
                started_at: created,
                updated_at: created,
                ended_at: None,
                child_chat_id: None,
            });
        }
    }

    for run in snapshot {
        match run.tool_call_id.as_deref() {
            Some(tool_id) => {
                if let Some(&ix) = by_tool.get(tool_id) {
                    merge_snapshot(&mut out[ix], run, now_ms);
                } else {
                    // Snapshot run whose tool part is not in this transcript
                    // (part already folded away / a later turn) — insert by
                    // run id so it still renders.
                    out.push(from_snapshot(run, now_ms));
                }
            }
            // Message-mode activity (no parent tool call).
            None => out.push(from_snapshot(run, now_ms)),
        }
    }
    out
}

/// A durable Cypher child chat row of the selected parent (from the synced
/// `Chat.child` relation), reduced to what the Inspector needs: the child chat
/// id + the persisted relation metadata. This is the DURABLE truth for
/// reopenability — it survives an empty parent snapshot and a parent restart.
#[derive(Debug, Clone, PartialEq)]
pub struct DurableChildRow {
    pub chat_id: String,
    /// The parent `cypher.subagents.v1` run id (the extension's stable run id).
    pub parent_run_id: String,
    /// The parent tool call id this run answers to (durable link to the
    /// transcript part; matches doc-only rows after a restart).
    pub tool_call_id: Option<String>,
    pub agent: String,
    pub task: String,
    pub mode: SubagentRunMode,
    /// Child chat creation time (epoch ms) — synthesized rows order by it.
    pub created_at_ms: i64,
}

/// Execution truth of a child chat from its OWN session row: Working /
/// AwaitingInput → Running (or Stale past the staleness window), Errored →
/// Error, Idle → Done. The child session row is durable — the parent snapshot
/// and restart cannot erase it.
pub fn child_session_panel_status(
    status: SessionStatus,
    updated_at_ms: i64,
    now_ms: i64,
) -> PanelStatus {
    match status {
        SessionStatus::Working | SessionStatus::AwaitingInput => {
            if now_ms.saturating_sub(updated_at_ms) > SUBAGENT_STALE_MS {
                PanelStatus::Stale
            } else {
                PanelStatus::Running
            }
        }
        SessionStatus::Errored => PanelStatus::Error,
        SessionStatus::Idle => PanelStatus::Done,
    }
}

/// Merge the parent's durable child chat rows into the aggregated panel.
///
/// - Match by `parent_run_id` first (the extension's stable run id), then by
///   `tool_call_id` (so a doc-only row still links after the parent snapshot
///   went empty / the parent restarted — never two rows for one child).
/// - ALWAYS restore `child_chat_id`/agent/task/mode from the durable row, so a
///   completed child stays clickable.
/// - The child's OWN session row is the execution truth where available.
/// - A child row with no panel entry (parent transcript/snapshot no longer
///   carries the run) is synthesized so the child stays navigable.
pub fn merge_child_rows(
    mut entries: Vec<SubagentPanelEntry>,
    children: &[DurableChildRow],
    child_status: impl Fn(&str) -> Option<PanelStatus>,
) -> Vec<SubagentPanelEntry> {
    for child in children {
        // 1) Already linked (a live snapshot carried the child chat id).
        // 2) Matched by run id (snapshot runs use the parent run uuid).
        // 3) Matched by tool call id (doc-only rows after a restart).
        let linked = entries.iter_mut().find(|e| {
            e.child_chat_id.as_deref() == Some(child.chat_id.as_str())
                || e.run_id == child.parent_run_id
                || (child.tool_call_id.is_some()
                    && e.tool_call_id.as_deref() == child.tool_call_id.as_deref())
        });
        if let Some(entry) = linked {
            // The durable relation always wins for the row's identity fields.
            entry.run_id = child.parent_run_id.clone();
            entry.child_chat_id = Some(child.chat_id.clone());
            entry.agent = child.agent.clone();
            entry.task = child.task.clone();
            entry.mode = child.mode;
            if let Some(status) = child_status(&child.chat_id) {
                entry.status = status;
            }
            continue;
        }
        // Synthesize: the parent side no longer knows this run, but the child
        // row is durable — keep it navigable. Without a session the run is
        // still in-flight (queued); with one, the session is the truth.
        let status = child_status(&child.chat_id).unwrap_or(PanelStatus::Starting);
        entries.push(SubagentPanelEntry {
            run_id: child.parent_run_id.clone(),
            tool_call_id: child.tool_call_id.clone(),
            agent: child.agent.clone(),
            task: child.task.clone(),
            model: None,
            mode: child.mode,
            status,
            progress: None,
            started_at: child.created_at_ms,
            updated_at: child.created_at_ms,
            ended_at: None,
            child_chat_id: Some(child.chat_id.clone()),
        });
    }
    entries
}

/// The next focused inspector row index under Up/Down: wraps, and a `None`
/// current row starts at the first (Down) / last (Up). Pure selection logic.
pub fn next_active_index(current: Option<usize>, delta: i32, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let start = match current {
        Some(i) => i as i64,
        None if delta > 0 => -1,
        None => len as i64,
    };
    Some((start + delta as i64).rem_euclid(len as i64) as usize)
}

/// Order for the inspector: in-flight (running/stale/starting) first by
/// started_at, then settled by ended_at desc (most recent first). No
/// truncation — the inspector scrolls; the cap lives in the viewport.
pub fn order_panel(mut entries: Vec<SubagentPanelEntry>) -> Vec<SubagentPanelEntry> {
    entries.sort_by(|a, b| {
        let af = SubagentPanelEntry::in_flight(a.status);
        let bf = SubagentPanelEntry::in_flight(b.status);
        match (af, bf) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => a
                .started_at
                .cmp(&b.started_at)
                .then_with(|| a.run_id.cmp(&b.run_id)),
            (false, false) => b
                .ended_at
                .unwrap_or(b.updated_at)
                .cmp(&a.ended_at.unwrap_or(a.updated_at))
                .then_with(|| a.run_id.cmp(&b.run_id)),
        }
    });
    entries
}

/// Header counts over the aggregated entries, split by derived status.
pub fn panel_counts(entries: &[SubagentPanelEntry]) -> PanelCounts {
    let mut counts = PanelCounts::default();
    for e in entries {
        match e.status {
            PanelStatus::Running => counts.running += 1,
            PanelStatus::Stale => counts.stale += 1,
            PanelStatus::Starting => counts.starting += 1,
            PanelStatus::Done => counts.done += 1,
            PanelStatus::Error => counts.failed += 1,
        }
    }
    counts
}

/// Collapsed-trigger copy. Priority: real `Running` leads with the accent `●`
/// (starting/stale/failed appended — starting and stale are NOT folded into
/// the running number); no running but Starting leads with the faint `◌`;
/// then quiet-only staleness `⚠`, failures `✕`, and a fully settled chat
/// reads as a success `✓`. Returns `(glyph, label)` — the glyph is the ONLY
/// colored part, the label stays muted.
pub fn trigger_label(counts: &PanelCounts) -> (&'static str, String) {
    if counts.running > 0 {
        let mut label = format!("{} running", counts.running);
        if counts.starting > 0 {
            label.push_str(&format!(" · {} starting", counts.starting));
        }
        if counts.stale > 0 {
            label.push_str(&format!(" · {} stale", counts.stale));
        }
        if counts.failed > 0 {
            label.push_str(&format!(" · ! {}", counts.failed));
        }
        ("●", label)
    } else if counts.starting > 0 {
        let mut label = format!("{} starting", counts.starting);
        if counts.stale > 0 {
            label.push_str(&format!(" · {} stale", counts.stale));
        }
        if counts.failed > 0 {
            label.push_str(&format!(" · ! {}", counts.failed));
        }
        ("◌", label)
    } else if counts.stale > 0 {
        ("⚠", format!("{} stale", counts.stale))
    } else if counts.failed > 0 {
        ("✕", format!("{} failed", counts.failed))
    } else {
        ("✓", format!("{} subagents", counts.done))
    }
}

/// The trigger glyph's color by its copy kind — the only colored part of the
/// collapsed accessory (accent/success/danger/warning).
fn trigger_color(glyph: &str, theme: &Theme) -> gpui::Hsla {
    match glyph {
        "⚠" => theme.warning,
        "✕" => theme.danger,
        "✓" => theme.success,
        "◌" => theme.text_faint,
        _ => theme.accent,
    }
}

/// An inspector open for `open_chat` must close when the selection moves to a
/// different chat (or to none). Pure — drives the chat-switch close in the
/// panel's state observation.
pub fn chat_switch_closes(open_chat: Option<&str>, selected_chat: Option<&str>) -> bool {
    match (open_chat, selected_chat) {
        (Some(open), Some(selected)) => open != selected,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

// ---------------------------------------------------------------------------
// GPUI panel
// ---------------------------------------------------------------------------

/// The Subagents chrome: a compact trigger on the status strip's right edge
/// (glyph + one-line summary, transparent until hover) that opens an UPWARD
/// inspector popover — right edge aligned with the trigger, 6px above it, in
/// a floating layer that never participates in the bottom-stack measurement.
/// UI memory only — the open chat / popup lifecycle is never persisted.
pub struct SubagentsPanel {
    state: Entity<AppState>,
    /// Open inspector lifecycle, keyed by the chat it was opened for — the
    /// value lets the state observation detect a chat switch (and close the
    /// old popover) while keeping the `popover::Popup` open→closing→reaped
    /// exit animation.
    popup: Popup<String>,
    /// Keyboard focus for the trigger (click/Enter/Space toggle, Escape
    /// close); closing restores composer focus through the shell's
    /// focus-lost fallback.
    focus: FocusHandle,
    /// Keyboard focus for the OPEN inspector's row list: real focused-row
    /// navigation — the list is `track_focus`'d and Up/Down move a tracked
    /// active row, Enter/Space open it, Escape closes. Distinct from the
    /// trigger's handle (two elements can never share one).
    list_focus: FocusHandle,
    /// The focused row index within the ordered inspector list (`None` until
    /// the user navigates). UI memory only.
    active_row: Option<usize>,
    /// Re-render when AppState changes (transcript/sessions frames).
    _state_observation: Subscription,
}

impl SubagentsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let _state_observation = cx.observe(&state, |this, state, cx| {
            // Chat switch closes the old inspector (never the new chat's).
            let open = this.popup.get().map(|id| id.clone());
            let selected = state.read(cx).selected_chat.clone();
            if chat_switch_closes(open.as_deref(), selected.as_deref()) && this.popup.begin_close()
            {
                popover::reap_popup(cx, |this: &mut Self| &mut this.popup);
            }
            cx.notify();
        });
        Self {
            state,
            popup: Popup::default(),
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            active_row: None,
            _state_observation,
        }
    }

    /// Close through the exit animation. Best-effort keyboard restore: when
    /// the focused trigger initiated the close (Escape), blur hands focus
    /// back to the composer (the shell's focus-lost fallback); pointer-driven
    /// closes leave focus where the click landed. Any focus inside the
    /// inspector (the tracked row list) is released too.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.popup);
        }
        self.active_row = None;
        if self.focus.is_focused(window) || self.list_focus.is_focused(window) {
            window.blur();
        }
        cx.notify();
    }

    /// Toggle the inspector for the current chat. The trigger press note
    /// distinguishes "this press dismissed it; stay closed" from "open
    /// fresh" (the `popover::Popup` contract — the anchored card's
    /// `on_mouse_down_out` closes on the same press the click would reopen).
    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.take_press_was_open() {
            self.close(window, cx);
            return;
        }
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        if self.popup.as_open().is_some_and(|id| id == &chat_id) {
            self.close(window, cx);
        } else {
            self.popup.open(chat_id);
            self.active_row = None;
            // Land keyboard focus on the tracked inspector list so Up/Down /
            // Enter / Escape work immediately after opening (the handle is
            // focusable before the mounted list's first paint).
            window.focus(&self.list_focus, cx);
            cx.notify();
        }
    }

    /// The expanded inspector: a header ("Subagents" + full split counts,
    /// always neutral) over the full ordered row list in ONE shared surface
    /// (hairline separators, no nested cards), scrolled. Width
    /// `min(520, main column − 32)` — a fixed width capped by the window,
    /// since a floating layer has no ancestor to resolve `w_full` against.
    /// Open a Cypher child chat from the inspector: close the popover (the
    /// exit animation reaps it) and select the child through the normal
    /// `AppState::select_chat` path. The Shell's NavHistory observation
    /// records the switch, so Back returns to this parent session.
    fn open_child_chat(&mut self, child_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.popup);
        }
        self.active_row = None;
        // Keyboard focus must not stay on the now-closing inspector row — the
        // shell's focus-lost fallback routes it back to the composer.
        if self.focus.is_focused(window) || self.list_focus.is_focused(window) {
            window.blur();
        }
        let state = self.state.clone();
        state.update(cx, |state, cx| state.select_chat(Some(child_id), cx));
        cx.notify();
    }

    /// Move the focused inspector row by `delta` (wrapping), landing keyboard
    /// focus on the tracked list so the keys keep coming. Pure selection logic
    /// lives in [`next_active_index`].
    fn move_active(&mut self, delta: i32, len: usize, window: &mut Window, cx: &mut Context<Self>) {
        if len == 0 {
            return;
        }
        self.active_row = next_active_index(self.active_row, delta, len);
        if !self.list_focus.is_focused(window) {
            window.focus(&self.list_focus, cx);
        }
        cx.notify();
    }

    fn inspector(
        &mut self,
        window: &Window,
        theme: &Theme,
        counts: &PanelCounts,
        ordered: &[SubagentPanelEntry],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let window_w = f32::from(window.viewport_size().width);
        let width = (520.0_f32).min((window_w - 32.0).max(280.0));
        let active = self.active_row;
        // Owned copy of the ordered rows' child ids so the key listener below
        // (a 'static closure) can open the active row without borrowing `ordered`.
        let child_ids: Vec<Option<String>> =
            ordered.iter().map(|e| e.child_chat_id.clone()).collect();

        let mut rows = div().flex().flex_col();
        for (ix, entry) in ordered.iter().enumerate() {
            if ix > 0 {
                rows = rows.child(
                    div()
                        .h(px(1.0))
                        .flex_none()
                        .bg(crate::theme::hairline(0.05)),
                );
            }
            let row = inspector_row(theme, entry);
            rows = rows.child(match entry.child_chat_id.clone() {
                // Cypher-hosted child runs are navigable. Keyboard navigation
                // is REAL focused-row navigation on the tracked list (Up/Down
                // move `active_row`, Enter/Space open the active row, Escape
                // closes) — the rows themselves are click targets only, never
                // individually focused. Clicking (or activating) closes the
                // inspector and selects the child's REAL live session through
                // the normal AppState::select_chat path — the Shell's
                // NavHistory observation records the switch, so Back returns
                // to this parent session.
                Some(child_id) => {
                    let id = format!("subagent-open-{}", entry.run_id);
                    let hover_id = id.clone();
                    let click_child = child_id.clone();
                    let mut row_wrap = div()
                        .id(id)
                        .cursor_pointer()
                        .on_hover(motion::hover_listener(&hover_id))
                        .bg(motion::hover_blend(
                            &hover_id,
                            gpui::transparent_black(),
                            crate::theme::ink(0.05),
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let child = click_child.clone();
                            this.open_child_chat(child, window, cx);
                        }));
                    if active == Some(ix) {
                        // The focused row highlight (distinct from hover).
                        row_wrap = row_wrap.bg(crate::theme::ink(0.09));
                    }
                    row_wrap.child(row).into_any_element()
                }
                None => {
                    let row_wrap = div();
                    if active == Some(ix) {
                        row_wrap
                            .bg(crate::theme::ink(0.09))
                            .child(row)
                            .into_any_element()
                    } else {
                        row.into_any_element()
                    }
                }
            });
        }

        let header = div()
            .h(px(34.0))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from("Subagents")),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme.text_muted)
                    .child(counts_line(counts)),
            );

        // Outside-click + Escape close (the established popover pattern);
        // rows live in a max_h 197px scroll region → the whole card tops out
        // at ~232px (34 header + 1 hairline + 197). The tracked list focus is
        // where real focused-row keyboard navigation happens: Up/Down move
        // the active row, Enter/Space open the active navigable row.
        let ordered_len = ordered.len();
        popover::popover_card_flush(theme)
            .w(px(width))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" => this.close(window, cx),
                    "up" => this.move_active(-1, ordered_len, window, cx),
                    "down" => this.move_active(1, ordered_len, window, cx),
                    "enter" | " " => {
                        if let Some(child) = this
                            .active_row
                            .and_then(|ix| child_ids.get(ix))
                            .cloned()
                            .flatten()
                        {
                            this.open_child_chat(child, window, cx);
                        }
                    }
                    _ => {}
                }
            }))
            .child(header)
            .child(
                div()
                    .h(px(1.0))
                    .flex_none()
                    .bg(crate::theme::hairline(0.07)),
            )
            .child(
                div()
                    .id("subagents-inspector-list")
                    .track_focus(&self.list_focus)
                    .max_h(px(197.0))
                    .overflow_y_scroll()
                    .child(rows),
            )
            .into_any_element()
    }
}

/// "· N running · N stale · N async · N done · N failed" — nonzero parts only.
fn counts_line(counts: &PanelCounts) -> SharedString {
    let mut parts: Vec<String> = Vec::new();
    if counts.running > 0 {
        parts.push(format!("{} running", counts.running));
    }
    if counts.stale > 0 {
        parts.push(format!("{} stale", counts.stale));
    }
    if counts.starting > 0 {
        parts.push(format!("{} starting", counts.starting));
    }
    if counts.done > 0 {
        parts.push(format!("{} done", counts.done));
    }
    if counts.failed > 0 {
        parts.push(format!("{} failed", counts.failed));
    }
    SharedString::from(if parts.is_empty() {
        String::new()
    } else {
        format!("· {}", parts.join(" · "))
    })
}

/// (glyph, color) for one status — the glyph tile carries the status color;
/// the row/header text stays neutral (error tints only the glyph).
fn status_glyph(status: PanelStatus, theme: &Theme) -> (&'static str, gpui::Hsla) {
    match status {
        PanelStatus::Running => ("⠋", theme.accent),
        PanelStatus::Stale => ("⚠", theme.warning),
        PanelStatus::Starting => ("◌", theme.text_faint),
        PanelStatus::Done => ("✓", theme.success),
        PanelStatus::Error => ("✗", theme.danger),
    }
}

/// One inspector row: a status glyph tile + agent + mode/model + one-line
/// task (the transcript `chip_header_row` language), with a second mono line
/// for in-flight entries' live progress tail (`live_progress_card`
/// language). Settled rows are ~34px, in-flight ~50px. Rows share the
/// inspector's single surface — separators are drawn between them, never
/// cards around them.
fn inspector_row(theme: &Theme, entry: &SubagentPanelEntry) -> gpui::Div {
    let (glyph, glyph_color) = status_glyph(entry.status, theme);
    let in_flight = SubagentPanelEntry::in_flight(entry.status);
    let mode_badge = match entry.mode {
        SubagentRunMode::Async => Some(SharedString::from("async")),
        SubagentRunMode::Message => Some(SharedString::from("msg")),
        SubagentRunMode::Sync => None,
    };
    let model: SharedString = entry.model.clone().unwrap_or_default().into();
    let task_head: SharedString = entry
        .task
        .lines()
        .next()
        .unwrap_or(&entry.task)
        .trim()
        .into();

    let head = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            // Status glyph tile (the chip header's icon-tile language).
            div()
                .size(px(18.0))
                .flex_none()
                .rounded(px(5.0))
                .bg(crate::theme::ink(0.06))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.0))
                .text_color(glyph_color)
                .child(SharedString::from(glyph)),
        )
        .child(
            div()
                .flex_none()
                .max_w(px(140.0))
                .truncate()
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(SharedString::from(&entry.agent)),
        )
        .when_some(mode_badge, |el, badge| {
            el.child(
                div()
                    .flex_none()
                    .rounded_full()
                    .px(px(5.0))
                    .py(px(1.0))
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(9.5))
                    .text_color(theme.text_faint)
                    .child(badge),
            )
        })
        .when(!model.is_empty(), |el| {
            el.child(
                div()
                    .flex_none()
                    .max_w(px(160.0))
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(model),
            )
        })
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(task_head),
        )
        .when(entry.is_navigable(), |el| {
            // Subtle chevron affordance: the row opens the child's live session.
            el.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from("›")),
            )
        });

    let mut row = div()
        .h(px(if in_flight { 50.0 } else { 34.0 }))
        .w_full()
        .flex_none()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(4.0))
        .px(px(12.0))
        .child(head);

    // In-flight entries show the live progress tail (mono, single line, dim),
    // indented under the task past the glyph tile.
    if in_flight && let Some(progress) = entry.progress.as_deref() {
        let tail = progress.lines().last().unwrap_or(progress).trim();
        if !tail.is_empty() {
            row = row.child(
                div()
                    .pl(px(26.0))
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(tail)),
            );
        }
    }
    row
}

impl Render for SubagentsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let now_ms = Utc::now().timestamp_millis();
        let state = self.state.read(cx);
        let Some(chat_id) = state.selected_chat.clone() else {
            return Empty.into_any_element();
        };
        let snapshot: Vec<SubagentRun> = state
            .session_for(&chat_id)
            .map(|s| s.subagents.clone())
            .unwrap_or_default();
        // Merge the parent's DURABLE child chat rows (the synced `Chat.child`
        // relation) into the aggregation: they keep completed children
        // navigable even after the parent's live snapshot goes empty or the
        // parent restarts, and the child's own session row is the execution
        // truth where available.
        let children: Vec<DurableChildRow> = state
            .chats
            .iter()
            .filter_map(|c| {
                let child = c.child.as_ref()?;
                if child.parent_chat_id != chat_id {
                    return None;
                }
                Some(DurableChildRow {
                    chat_id: c.id.clone(),
                    parent_run_id: child.parent_run_id.clone(),
                    tool_call_id: child.tool_call_id.clone(),
                    agent: child.agent.clone(),
                    task: child.task.clone(),
                    mode: child.mode,
                    created_at_ms: c.created_at.timestamp_millis(),
                })
            })
            .collect();
        let child_status = |child_id: &str| {
            state.session_for(child_id).map(|s| {
                child_session_panel_status(s.status, s.updated_at.timestamp_millis(), now_ms)
            })
        };
        let entries = merge_child_rows(
            aggregate_subagents(&state.transcript, &snapshot, now_ms),
            &children,
            child_status,
        );
        // No records → no trigger at all.
        if entries.is_empty() {
            return Empty.into_any_element();
        }
        let counts = panel_counts(&entries);
        let (glyph, label) = trigger_label(&counts);
        let glyph_color = trigger_color(glyph, &theme);

        // Open OR playing the exit animation (get(), not as_open()) — the
        // anchored layer must stay mounted while the close eases out.
        let open = self.popup.get().is_some_and(|id| id == &chat_id);

        let mut trigger = div()
            .id("subagents-trigger")
            .track_focus(&self.focus)
            .h(px(22.0))
            .min_w_0()
            .px(px(8.0))
            .rounded_full()
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_pointer()
            // Transparent at rest; a faint ink wash on hover — never a
            // permanent border or background.
            .bg(motion::hover_blend(
                "subagents-trigger",
                crate::theme::ink(0.0),
                crate::theme::ink(0.06),
            ))
            .on_hover(motion::hover_listener("subagents-trigger"))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                match ev.keystroke.key.as_str() {
                    "enter" | " " => {
                        this.toggle(window, cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.close(window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    this.popup.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.toggle(window, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(glyph_color)
                    .child(SharedString::from(glyph)),
            )
            .child(
                div()
                    .min_w_0()
                    .max_w(px(150.0))
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(label)),
            );

        if open {
            let closing = self.popup.closing_since();
            let ordered = order_panel(entries);
            let inspector = self.inspector(window, &theme, &counts, &ordered, cx);
            trigger = trigger.relative().child(popover::anchored_menu_above_end(
                "subagents-inspector",
                inspector,
                closing,
            ));
        }

        trigger.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::ToolCall;

    fn ms(epoch_secs: i64) -> i64 {
        epoch_secs * 1000
    }

    fn subagent_part(id: &str, is_async: bool, resolved: bool, is_error: bool) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call: ToolCall::Unknown {
                name: "subagent".into(),
                input: Some(serde_json::json!({
                    "agent": if id.starts_with("planner") { "planner" } else { "actor" },
                    "task": "Plan the panel",
                    "async": is_async,
                })),
            },
            is_error,
            resolved,
            output: None,
            progress: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
        }
    }

    fn entry(parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: "m1".into(),
            role: cypher_doc::MessageRole::Assistant,
            parts,
            created_at: ms(1000),
            device_id: "d".into(),
            status: None,
            continuation_of: None,
        }
    }

    /// A settled entry (`status: None`) — the common case for a finished
    /// assistant message. An entry mid-stream is marked Streaming.
    fn streaming_entry(parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            status: Some(MessageStatus::Streaming),
            ..entry(parts)
        }
    }

    fn run(
        run_id: &str,
        tool_call_id: Option<&str>,
        mode: SubagentRunMode,
        status: SubagentRunStatus,
        updated_at: i64,
    ) -> SubagentRun {
        SubagentRun {
            run_id: run_id.into(),
            tool_call_id: tool_call_id.map(str::to_owned),
            agent: "planner".into(),
            model: Some("anthropic/claude-sonnet-4".into()),
            task: "Plan the panel".into(),
            mode,
            status,
            progress: Some("live tail".into()),
            started_at: ms(1000),
            updated_at,
            ended_at: if status == SubagentRunStatus::Running {
                None
            } else {
                Some(updated_at)
            },
            child_chat_id: None,
        }
    }

    /// Cross-turn aggregation: subagent tool parts from EVERY transcript
    /// entry merge into one panel (not just the last turn).
    #[test]
    fn aggregates_across_turns() {
        // Turn 1: an unresolved sync part on a STILL-STREAMING entry → Starting.
        // Turn 2: a settled resolved sync part → Done.
        let entries = vec![
            streaming_entry(vec![subagent_part("t1", false, false, false)]),
            entry(vec![subagent_part("t2", false, true, false)]),
        ];
        let now = ms(2000);
        let out = aggregate_subagents(&entries, &[], now);
        assert_eq!(out.len(), 2);
        let t1 = out
            .iter()
            .find(|e| e.tool_call_id.as_deref() == Some("t1"))
            .unwrap();
        assert_eq!(t1.status, PanelStatus::Starting);
        let t2 = out
            .iter()
            .find(|e| e.tool_call_id.as_deref() == Some("t2"))
            .unwrap();
        assert_eq!(t2.status, PanelStatus::Done);
    }

    /// Sync runs: the doc ToolResult terminal state wins even when the
    /// snapshot still says running; the snapshot supplements model/progress.
    #[test]
    fn sync_doc_terminal_wins_snapshot_supplements() {
        let entries = vec![entry(vec![subagent_part("t1", false, true, false)])];
        let now = ms(2000);
        let snapshot = vec![run(
            "r1",
            Some("t1"),
            SubagentRunMode::Sync,
            SubagentRunStatus::Running,
            now - 1000,
        )];
        let out = aggregate_subagents(&entries, &snapshot, ms(2000));
        let t1 = &out[0];
        // Doc says resolved+ok → Done, despite the snapshot still Running.
        assert_eq!(t1.status, PanelStatus::Done);
        assert_eq!(t1.model.as_deref(), Some("anthropic/claude-sonnet-4"));
        assert_eq!(t1.progress.as_deref(), Some("live tail"));
    }

    /// Async runs: the snapshot is the authority. A resolved async launch ack
    /// is IGNORED on its own; only the snapshot's Running/Done/Error reads.
    #[test]
    fn async_snapshot_is_authoritative_never_doc_ack() {
        let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
        let now = ms(2000);
        // Snapshot says running → Running (the doc ToolResult was only the ack).
        let snapshot = vec![run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now - 1000,
        )];
        let out = aggregate_subagents(&entries, &snapshot, now);
        assert_eq!(out[0].status, PanelStatus::Running);
        // Snapshot says done → Done.
        let snapshot = vec![run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Done,
            now - 1000,
        )];
        let out = aggregate_subagents(&entries, &snapshot, now);
        assert_eq!(out[0].status, PanelStatus::Done);
        // Snapshot says error → Error.
        let snapshot = vec![run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Error,
            now - 1000,
        )];
        let out = aggregate_subagents(&entries, &snapshot, now);
        assert_eq!(out[0].status, PanelStatus::Error);
    }

    /// A snapshot's Running whose freshness went quiet reads stale — the 45s
    /// window is a visual heartbeat guard, never a terminal judgment.
    #[test]
    fn stale_running_reads_stale() {
        let now = ms(2000);
        let entries = vec![streaming_entry(vec![subagent_part(
            "t1", true, false, false,
        )])];
        let snapshot = vec![run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            ms(1000) - SUBAGENT_STALE_MS - 1000,
        )];
        let out = aggregate_subagents(&entries, &snapshot, now);
        assert_eq!(out[0].status, PanelStatus::Stale);
        // A fresh snapshot stays Running.
        let snapshot = vec![run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        )];
        let out = aggregate_subagents(&entries, &snapshot, now);
        assert_eq!(out[0].status, PanelStatus::Running);
    }

    /// Snapshot merge by tool call id; message-mode runs (no tool call id)
    /// insert by run id alongside.
    #[test]
    fn snapshot_merges_by_tool_call_and_inserts_message_runs() {
        let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
        let now = ms(2000);
        let snapshot = vec![
            run(
                "r1",
                Some("t1"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                now,
            ),
            run(
                "msg-1",
                None,
                SubagentRunMode::Message,
                SubagentRunStatus::Running,
                now,
            ),
            run(
                "ghost",
                Some("t-gone"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                now,
            ),
        ];
        let out = aggregate_subagents(&entries, &snapshot, now);
        assert_eq!(out.len(), 3, "merged + message + orphan snapshot runs");
        assert!(
            out.iter()
                .any(|e| e.run_id == "msg-1" && e.mode == SubagentRunMode::Message)
        );
        assert!(out.iter().any(|e| e.run_id == "ghost"));
        assert_eq!(
            out.iter().find(|e| e.run_id == "r1").unwrap().status,
            PanelStatus::Running
        );
    }

    /// Missing task/agent degrade: a subagent part without agent yields no
    /// entry; a blank task still renders.
    #[test]
    fn missing_fields_do_not_panic() {
        let entries = vec![streaming_entry(vec![MessagePart::Tool {
            id: "t1".into(),
            call: ToolCall::Unknown {
                name: "subagent".into(),
                input: Some(serde_json::json!({ "agent": "planner" })),
            },
            is_error: false,
            resolved: false,
            output: None,
            progress: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
        }])];
        let out = aggregate_subagents(&entries, &[], ms(2000));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].task, "");
        assert_eq!(out[0].status, PanelStatus::Starting);
        // Non-subagent tool parts never produce rows.
        let entries = vec![entry(vec![MessagePart::Tool {
            id: "t2".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
            is_error: false,
            resolved: true,
            output: None,
            progress: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
        }])];
        assert!(aggregate_subagents(&entries, &[], ms(2000)).is_empty());
    }

    /// Ordering: in-flight rows first (by started_at), then settled by ended_at
    /// desc. The inspector SCROLLS — nothing is truncated away.
    #[test]
    fn ordering_puts_running_first_and_keeps_everything() {
        let entries = vec![
            entry(vec![subagent_part("a", false, true, false)]),
            entry(vec![subagent_part("b", false, true, false)]),
            entry(vec![subagent_part("c", true, true, false)]),
        ];
        // c is running (async + snapshot running); a and b are settled sync.
        let snapshot = vec![
            run(
                "r-c",
                Some("c"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                ms(2000) - 1000,
            ),
            run(
                "r-a",
                Some("a"),
                SubagentRunMode::Sync,
                SubagentRunStatus::Done,
                ms(2000) - 2000,
            ),
            run(
                "r-b",
                Some("b"),
                SubagentRunMode::Sync,
                SubagentRunStatus::Done,
                ms(2000) - 1000,
            ),
        ];
        let out = aggregate_subagents(&entries, &snapshot, ms(2000));
        let ordered = order_panel(out.clone());
        assert_eq!(ordered.len(), 3, "no truncation");
        assert_eq!(ordered[0].run_id, "r-c", "running first");
        // Settled by ended_at desc.
        assert_eq!(ordered[1].run_id, "r-b");
        assert_eq!(ordered[2].run_id, "r-a");

        // Many more settled rows: ALL survive, ordered, running still first.
        let mut more = out.clone();
        for i in 0..4 {
            more.push(SubagentPanelEntry {
                run_id: format!("extra-{i}"),
                tool_call_id: Some(format!("e{i}")),
                agent: "actor".into(),
                task: "T".into(),
                model: None,
                mode: SubagentRunMode::Sync,
                status: PanelStatus::Done,
                progress: None,
                started_at: ms(1000),
                updated_at: ms(1000),
                ended_at: Some(ms(2000) - 3000 + 200 * i as i64),
                child_chat_id: None,
            });
        }
        let ordered = order_panel(more);
        assert_eq!(ordered.len(), 7, "all rows kept for the scroller");
        assert!(SubagentPanelEntry::in_flight(ordered[0].status));
        // Settled block still ends at the OLDEST settled row.
        assert_eq!(ordered.last().unwrap().run_id, "extra-0");
    }

    #[test]
    fn counts_aggregate_running_done_failed() {
        let entries = vec![
            entry(vec![subagent_part("a", false, true, true)]),
            entry(vec![subagent_part("b", false, true, false)]),
            entry(vec![subagent_part("c", true, true, false)]),
        ];
        let snapshot = vec![run(
            "r-c",
            Some("c"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            ms(2000) - 1000,
        )];
        let out = aggregate_subagents(&entries, &snapshot, ms(2000));
        let counts = panel_counts(&out);
        assert_eq!(counts.running, 1);
        assert_eq!(counts.done, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.stale, 0);
        assert_eq!(counts.starting, 0);
    }

    /// The split counts distinguish stale runners, starting runs and settled
    /// work — the trigger/header copy relies on that distinction.
    #[test]
    fn counts_split_stale_starting_and_done() {
        let entries = vec![
            entry(vec![subagent_part("a", true, true, false)]),
            entry(vec![subagent_part("b", true, true, false)]),
            streaming_entry(vec![subagent_part("c", true, false, false)]),
            entry(vec![subagent_part("d", false, true, false)]),
            entry(vec![subagent_part("e", false, true, true)]),
        ];
        let now = ms(2000);
        let snapshot = vec![
            // a: fresh running.
            run(
                "r-a",
                Some("a"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                now,
            ),
            // b: quiet → stale.
            run(
                "r-b",
                Some("b"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                now - SUBAGENT_STALE_MS - 1000,
            ),
        ];
        let out = aggregate_subagents(&entries, &snapshot, now);
        let counts = panel_counts(&out);
        assert_eq!(counts.running, 1);
        assert_eq!(counts.stale, 1);
        // c is unresolved + streaming with no snapshot — Starting, never Done.
        assert_eq!(counts.starting, 1);
        assert_eq!(counts.done, 1);
        assert_eq!(counts.failed, 1);
    }

    /// Trigger copy priority: real running leads (starting/stale/failed are
    /// appended, never folded into the running number), then only-starting,
    /// then only-stale, then failed-only, then all-settled.
    #[test]
    fn trigger_label_priorities() {
        // Live running + starting + stale lead with `●`; failures appended.
        let counts = PanelCounts {
            running: 2,
            stale: 1,
            starting: 1,
            done: 3,
            failed: 1,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "●");
        assert_eq!(label, "2 running · 1 starting · 1 stale · ! 1");

        // No running but starting → faint `◌` (in-flight, never Done).
        let counts = PanelCounts {
            running: 0,
            stale: 1,
            starting: 2,
            done: 1,
            failed: 1,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "◌");
        assert_eq!(label, "2 starting · 1 stale · ! 1");

        // Only stale → warning.
        let counts = PanelCounts {
            running: 0,
            stale: 2,
            starting: 0,
            done: 1,
            failed: 0,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "⚠");
        assert_eq!(label, "2 stale");

        // No in-flight, but failures → danger.
        let counts = PanelCounts {
            running: 0,
            stale: 0,
            starting: 0,
            done: 0,
            failed: 3,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "✕");
        assert_eq!(label, "3 failed");

        // Fully settled → success.
        let counts = PanelCounts {
            running: 0,
            stale: 0,
            starting: 0,
            done: 5,
            failed: 0,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "✓");
        assert_eq!(label, "5 subagents");

        // Mixed: running + failures keeps the running lead with the appended
        // failure count — the running number never absorbs starting/stale.
        let counts = PanelCounts {
            running: 1,
            stale: 0,
            starting: 2,
            done: 2,
            failed: 1,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "●");
        assert_eq!(label, "1 running · 2 starting · ! 1");
    }

    /// Starting counts as in-flight in the trigger — but with no snapshot it
    /// reads as `◌ N starting`, never as running, never as Done.
    #[test]
    fn starting_is_in_flight_not_running_in_trigger() {
        let counts = PanelCounts {
            running: 0,
            stale: 0,
            starting: 1,
            done: 4,
            failed: 0,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "◌", "no snapshot → starting, not running");
        assert_eq!(label, "1 starting");
    }

    /// Test 1 — a resolved async launch ACK with NO snapshot aggregates to
    /// nothing: the doc-only ack must never ghost a run.
    #[test]
    fn resolved_async_ack_without_snapshot_is_ignored() {
        let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
        let out = aggregate_subagents(&entries, &[], ms(2000));
        assert!(out.is_empty(), "a launch ack with no snapshot is no run");
    }

    /// Test 2 — an unresolved part on a STILL-STREAMING entry reads Starting
    /// and never contributes to the running count.
    #[test]
    fn streaming_unresolved_reads_starting_not_running() {
        let entries = vec![streaming_entry(vec![subagent_part(
            "t1", false, false, false,
        )])];
        let out = aggregate_subagents(&entries, &[], ms(2000));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, PanelStatus::Starting);
        let counts = panel_counts(&out);
        assert_eq!(counts.running, 0);
        assert_eq!(counts.starting, 1);
    }

    /// Test 3 — an unresolved part on a SETTLED entry (Complete/Aborted) is
    /// ignored: a dead turn must not ghost a runner.
    #[test]
    fn nonstreaming_unresolved_is_ignored() {
        let entries = vec![entry(vec![subagent_part("t1", false, false, false)])];
        let out = aggregate_subagents(&entries, &[], ms(2000));
        assert!(out.is_empty(), "settled entry's unresolved part is a ghost");
    }

    /// Test 4 — a resolved async launch that FAILED (is_error) is a durable
    /// Error even with no snapshot: the launch itself failed.
    #[test]
    fn resolved_async_error_is_durable_error_fallback() {
        let entries = vec![entry(vec![subagent_part("t1", true, true, true)])];
        let out = aggregate_subagents(&entries, &[], ms(2000));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, PanelStatus::Error);
        let counts = panel_counts(&out);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.running, 0);
    }

    /// Test 5 — Running/Stale can ONLY come from a structured snapshot: the
    /// same doc part is Starting without one and Running with one.
    #[test]
    fn running_only_comes_from_snapshot() {
        let entries = vec![streaming_entry(vec![subagent_part(
            "t1", true, false, false,
        )])];
        // No snapshot: Starting, never Running.
        let out = aggregate_subagents(&entries, &[], ms(2000));
        assert_eq!(out[0].status, PanelStatus::Starting);
        assert_eq!(panel_counts(&out).running, 0);
        // Fresh snapshot on the same tool call: Running.
        let now = ms(2000);
        let out = aggregate_subagents(
            &entries,
            &[run(
                "r1",
                Some("t1"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                now,
            )],
            now,
        );
        assert_eq!(out[0].status, PanelStatus::Running);
        assert_eq!(panel_counts(&out).running, 1);
    }

    /// Test 6 — async snapshot Running/Done/Error is the authority over the
    /// doc ack (covered across three snapshots in
    /// [`async_snapshot_is_authoritative_never_doc_ack`]).
    #[test]
    fn async_snapshot_authority_covers_all_terminal_statuses() {
        let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
        let now = ms(2000);
        for (status, expected) in [
            (SubagentRunStatus::Running, PanelStatus::Running),
            (SubagentRunStatus::Done, PanelStatus::Done),
            (SubagentRunStatus::Error, PanelStatus::Error),
        ] {
            let out = aggregate_subagents(
                &entries,
                &[run("r1", Some("t1"), SubagentRunMode::Async, status, now)],
                now,
            );
            assert_eq!(
                out[0].status, expected,
                "async snapshot {status:?} is authority"
            );
        }
    }

    /// Test 7 — when a snapshot goes from running to EMPTY, the async ack is
    /// NOT resurrected: the aggregate is pure over the current inputs.
    #[test]
    fn snapshot_disappearing_does_not_resurrect_ack() {
        let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
        let now = ms(2000);
        let with_running = aggregate_subagents(
            &entries,
            &[run(
                "r1",
                Some("t1"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                now,
            )],
            now,
        );
        assert_eq!(with_running[0].status, PanelStatus::Running);
        // The same transcript with the snapshot gone → no run at all.
        let without = aggregate_subagents(&entries, &[], now);
        assert!(without.is_empty(), "an ack must not outlive its snapshot");
    }

    /// Test 8 — the trigger's running number is exactly the real Running
    /// count; starting/stale are appended, never folded in.
    #[test]
    fn trigger_running_number_excludes_starting_and_stale() {
        let counts = PanelCounts {
            running: 1,
            stale: 3,
            starting: 2,
            done: 1,
            failed: 0,
        };
        let (glyph, label) = trigger_label(&counts);
        assert_eq!(glyph, "●");
        assert_eq!(label, "1 running · 2 starting · 3 stale");
    }

    /// Chat switch closes the old inspector (the reducer the state
    /// observation drives).
    #[test]
    fn chat_switch_closes_the_old_inspector() {
        assert!(chat_switch_closes(Some("a"), Some("b")));
        assert!(chat_switch_closes(Some("a"), None));
        assert!(!chat_switch_closes(Some("a"), Some("a")));
        assert!(!chat_switch_closes(None, Some("b")));
        assert!(!chat_switch_closes(None, None));
    }

    /// The inspector's call-field parsing reads agent/task/async off the
    /// doc tool part (the privacy-safe reader shared with the transcript).
    #[test]
    fn subagent_call_info_parses_the_panel_fields() {
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(
                serde_json::json!({ "agent": "planner", "task": "Plan\nmore", "async": true }),
            ),
        };
        let info = subagent_call_info(&call).expect("extracts");
        assert_eq!(info.agent, "planner");
        assert_eq!(info.task, "Plan\nmore");
        assert!(info.is_async);
    }

    /// The Cypher child chat id rides the snapshot run into the panel entry:
    /// a run with `childChatId` reads navigable (the row opens the child's
    /// live session), and a run without one never does (standalone spawns).
    #[test]
    fn child_chat_id_propagates_and_marks_navigable() {
        let entries = vec![entry(vec![subagent_part("t1", true, false, false)])];
        let now = ms(2000);
        let mut navigable = run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        );
        navigable.child_chat_id = Some("child-1".into());
        let out = aggregate_subagents(&entries, &[navigable], now);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_navigable());
        assert_eq!(out[0].child_chat_id.as_deref(), Some("child-1"));

        // A plain run (no child chat) stays non-navigable.
        let out = aggregate_subagents(
            &entries,
            &[run(
                "r2",
                Some("t1"),
                SubagentRunMode::Async,
                SubagentRunStatus::Running,
                now,
            )],
            now,
        );
        assert_eq!(out[0].child_chat_id, None);
        assert!(!out[0].is_navigable());

        // Snapshot-only runs (no transcript part) keep the id too.
        let out = aggregate_subagents(
            &[],
            &[SubagentRun {
                run_id: "r3".into(),
                tool_call_id: None,
                agent: "planner".into(),
                model: None,
                task: "T".into(),
                mode: SubagentRunMode::Message,
                status: SubagentRunStatus::Running,
                progress: None,
                started_at: now,
                updated_at: now,
                ended_at: None,
                child_chat_id: Some("child-3".into()),
            }],
            now,
        );
        assert_eq!(out[0].child_chat_id.as_deref(), Some("child-3"));
        assert!(out[0].is_navigable());
    }

    // ---- durable child rows (CRITICAL: reopenability/status truth) ----

    fn child_row(
        chat_id: &str,
        parent_run_id: &str,
        tool_call_id: Option<&str>,
        created_at: i64,
    ) -> DurableChildRow {
        DurableChildRow {
            chat_id: chat_id.into(),
            parent_run_id: parent_run_id.into(),
            tool_call_id: tool_call_id.map(str::to_owned),
            agent: "planner".into(),
            task: "Plan the panel".into(),
            mode: SubagentRunMode::Async,
            created_at_ms: created_at,
        }
    }

    /// A completed child stays navigable after an EMPTY parent snapshot: the
    /// durable child row synthesizes the run (child session Idle → Done).
    #[test]
    fn child_rows_keep_completed_children_navigable_after_empty_snapshot() {
        let now = ms(2000);
        // Async launch ack with no snapshot: the plain aggregate drops it.
        let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
        let base = aggregate_subagents(&entries, &[], now);
        assert!(base.is_empty(), "an ack must not outlive its snapshot");
        // The durable child row restores the run and keeps it clickable.
        let children = vec![child_row("child-1", "run-1", Some("t1"), ms(1000))];
        let out = merge_child_rows(base, &children, |_| Some(PanelStatus::Done));
        assert_eq!(out.len(), 1);
        assert!(out[0].is_navigable());
        assert_eq!(out[0].child_chat_id.as_deref(), Some("child-1"));
        assert_eq!(out[0].run_id, "run-1");
        assert_eq!(out[0].status, PanelStatus::Done);
        assert_eq!(out[0].tool_call_id.as_deref(), Some("t1"));
    }

    /// After a parent RESTART the sync doc row (run id = tool call id) links
    /// to the durable child by `tool_call_id` — exactly ONE navigable row,
    /// never a doc row plus a synthesized twin.
    #[test]
    fn child_rows_link_doc_rows_to_children_after_restart() {
        let now = ms(2000);
        let entries = vec![entry(vec![subagent_part("t1", false, true, false)])];
        let base = aggregate_subagents(&entries, &[], now);
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].run_id, "t1", "doc-only row keys by tool call id");
        assert!(!base[0].is_navigable());
        let children = vec![child_row("child-1", "run-1", Some("t1"), ms(1000))];
        let out = merge_child_rows(base, &children, |_| Some(PanelStatus::Done));
        assert_eq!(out.len(), 1, "no duplicate row for one child");
        assert_eq!(out[0].run_id, "run-1", "renamed to the durable run id");
        assert!(out[0].is_navigable());
        assert_eq!(out[0].child_chat_id.as_deref(), Some("child-1"));
        assert_eq!(out[0].status, PanelStatus::Done);
    }

    /// The child's OWN session row is the execution truth for a merged row.
    #[test]
    fn child_rows_use_child_session_as_execution_truth() {
        let children = vec![child_row("child-1", "run-1", None, ms(1000))];
        assert_eq!(
            merge_child_rows(vec![], &children, |_| Some(PanelStatus::Running))[0].status,
            PanelStatus::Running
        );
        assert_eq!(
            merge_child_rows(vec![], &children, |_| Some(PanelStatus::Error))[0].status,
            PanelStatus::Error
        );
        assert_eq!(
            merge_child_rows(vec![], &children, |_| Some(PanelStatus::Done))[0].status,
            PanelStatus::Done
        );
        // No session row yet → the run is still in-flight (queued), never a
        // false terminal.
        assert_eq!(
            merge_child_rows(vec![], &children, |_| None)[0].status,
            PanelStatus::Starting
        );
    }

    /// The durable relation wins on a matched row, and the child session
    /// overrides a stale parent-side status (e.g. a leftover Running edge).
    #[test]
    fn child_rows_durable_relation_wins_and_session_overrides_status() {
        let now = ms(2000);
        let entries = vec![entry(vec![subagent_part("t1", false, false, false)])];
        let snapshot = vec![run(
            "run-1",
            Some("t1"),
            SubagentRunMode::Sync,
            SubagentRunStatus::Running,
            now,
        )];
        let base = aggregate_subagents(&entries, &snapshot, now);
        assert!(!base[0].is_navigable());
        let children = vec![child_row("child-1", "run-1", Some("t1"), ms(1000))];
        let out = merge_child_rows(base, &children, |_| Some(PanelStatus::Done));
        assert_eq!(out.len(), 1);
        assert!(out[0].is_navigable());
        assert_eq!(out[0].status, PanelStatus::Done, "child session wins");
        assert_eq!(out[0].agent, "planner");
        assert_eq!(out[0].task, "Plan the panel");
    }

    /// Child session status mapping: Working/AwaitingInput → Running (Stale
    /// past the staleness window), Errored → Error, Idle → Done.
    #[test]
    fn child_session_status_maps_to_panel() {
        let now = ms(2000);
        assert_eq!(
            child_session_panel_status(SessionStatus::Working, now, now),
            PanelStatus::Running
        );
        assert_eq!(
            child_session_panel_status(SessionStatus::AwaitingInput, now, now),
            PanelStatus::Running
        );
        assert_eq!(
            child_session_panel_status(SessionStatus::Working, now - SUBAGENT_STALE_MS - 1000, now),
            PanelStatus::Stale
        );
        assert_eq!(
            child_session_panel_status(SessionStatus::Errored, now, now),
            PanelStatus::Error
        );
        assert_eq!(
            child_session_panel_status(SessionStatus::Idle, now, now),
            PanelStatus::Done
        );
    }

    /// Focused-row selection: Down starts at the first row, Up at the last,
    /// and movement wraps at both ends.
    #[test]
    fn next_active_index_wraps_and_starts_at_ends() {
        assert_eq!(next_active_index(None, 1, 3), Some(0));
        assert_eq!(next_active_index(None, -1, 3), Some(2));
        assert_eq!(next_active_index(Some(0), 1, 3), Some(1));
        assert_eq!(next_active_index(Some(2), 1, 3), Some(0), "wraps forward");
        assert_eq!(next_active_index(Some(0), -1, 3), Some(2), "wraps backward");
        assert_eq!(next_active_index(Some(1), 0, 3), Some(1));
        assert_eq!(next_active_index(None, 1, 0), None);
        assert_eq!(next_active_index(Some(0), 1, 0), None);
    }
}
