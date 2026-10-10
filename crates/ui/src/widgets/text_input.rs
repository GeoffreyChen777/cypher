//! The app's text input (adapted from gpui examples/input.rs): multiline
//! editing with selection, IME, undo and auto-grow, used by the composer,
//! settings fields and palette searches. Key bindings live under the
//! `Composer`, `ProviderField`, `PromptField` and `PaletteSearch` contexts;
//! an owner can project the text through chips ([`TextProjection`]).

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollWheelEvent, SharedString, Task, TextRun, TextStyle,
    UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, point, prelude::*, px, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::kit::theme::Theme;

mod editing;
mod element;
mod geometry;
mod ime;
mod projection;

pub use projection::*;

actions!(
    composer,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Copy,
        Cut,
        Paste,
        Newline,
        Submit,
        Undo,
        Redo,
        MentionTab,
        MentionEscape,
    ]
);

/// Expanded-mode textarea vertical padding: `pt-4 pb-1` (zeron composer.tsx
/// line 578) = 16 + 4.
pub const TEXTAREA_PAD_V: f32 = 20.0;
/// The expanded textarea BOX (content + padding) is clamped by the original's
/// auto-grow effect: `ta.style.height = Math.min(Math.max(scrollHeight, 76),
/// 260)` (zeron composer.tsx line 235). The 76px floor applies even when
/// empty — it's what makes the always-expanded new-chat composer tall.
pub const TEXTAREA_MIN: f32 = 76.0;
pub const TEXTAREA_MAX: f32 = 260.0;

/// Input text metrics: `text-[14px] leading-relaxed` = 14 × 1.625 = 22.75.
pub const INPUT_LINE_HEIGHT: f32 = 22.75;
pub const INPUT_TEXT_SIZE: f32 = 14.0;
/// Content cap for a [`TextInput::settings_prompt_field`] — a settings
/// field that holds a DOCUMENT (a subagent's system prompt) rather than a
/// value. Deep enough to read a paragraph in place, then scrolls internally.
pub const PROMPT_FIELD_MAX: f32 = 420.0;

/// Caret blink half-period (standard textarea cadence: ~500ms on / 500ms off).
pub const CARET_BLINK_MS: u64 = 500;

/// Caret blink phase for a time since the last keystroke/caret move: solid
/// through the first half-period (typing bursts never blink — each keystroke
/// resets the phase), then alternating.
pub fn caret_visible(ms_since_activity: u64) -> bool {
    (ms_since_activity / CARET_BLINK_MS).is_multiple_of(2)
}

/// Drag-selection autoscroll runs at the display-friendly 60fps cadence.
pub const DRAG_SCROLL_FRAME_MS: u64 = 16;

pub fn input_max_scroll(content_height: f32, viewport_height: f32) -> f32 {
    (content_height - viewport_height).max(0.0)
}

/// Apply GPUI's wheel delta to a top-origin input offset. Positive deltas mean
/// scrolling toward the start, matching gpui's built-in list/div behavior.
pub fn input_scroll_offset(
    current: f32,
    delta_y: f32,
    content_height: f32,
    viewport_height: f32,
) -> f32 {
    (current - delta_y).clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// Minimally adjust the viewport so the caret row is fully visible.
pub fn input_scroll_offset_for_cursor(
    current: f32,
    cursor_top: f32,
    cursor_height: f32,
    content_height: f32,
    viewport_height: f32,
) -> f32 {
    let mut next = current;
    if cursor_top < next {
        next = cursor_top;
    } else if cursor_top + cursor_height > next + viewport_height {
        next = cursor_top + cursor_height - viewport_height;
    }
    next.clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// Per-frame drag-selection scroll. Distance increases speed, capped at one
/// text row per frame so crossing the input boundary never causes a jump.
pub fn input_drag_scroll_delta(
    pointer_y: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    line_height: f32,
) -> f32 {
    let distance = if pointer_y < viewport_top {
        pointer_y - viewport_top
    } else if pointer_y > viewport_bottom {
        pointer_y - viewport_bottom
    } else {
        return 0.0;
    };
    distance.signum() * (distance.abs() * 0.2).clamp(1.0, line_height)
}

/// How long a run of single-character edits keeps merging into one undo step.
/// A pause longer than this starts a fresh step, so undo rewinds in the
/// bursts the user actually typed rather than one character at a time.
const UNDO_COALESCE: Duration = Duration::from_millis(700);

/// Cap on retained undo steps — a long-lived input must not grow forever.
const UNDO_LIMIT: usize = 200;

/// A restorable point in the input's history: text plus where the caret and
/// selection sat when the edit landed.
#[derive(Clone)]
struct EditSnapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

/// Direction of the last edit — a run only merges with edits of its own kind.
#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Insert,
    Delete,
}

/// Bind the text input keymap. Call once at app boot.
pub fn init(cx: &mut App) {
    // Settings fields share native text editing, not the chat's Tab/Escape
    // completions or Shift+Enter multiline behavior.
    let ctx = Some("Composer || ProviderField || PromptField");
    // Enter SUBMITS a single-line value field, but a prompt field holds a
    // document: there, both Enter and Shift+Enter insert a line break and
    // saving is an explicit button.
    let submit_ctx = Some("Composer || ProviderField");
    let mut bindings = vec![
        KeyBinding::new("enter", Submit, submit_ctx),
        KeyBinding::new("enter", Newline, Some("PromptField")),
        KeyBinding::new("shift-enter", Newline, Some("PromptField")),
        KeyBinding::new("tab", MentionTab, Some("Composer")),
        KeyBinding::new("escape", MentionEscape, Some("Composer")),
        KeyBinding::new("shift-enter", Newline, Some("Composer")),
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        // macOS line/document motion — a laptop keyboard has no home/end keys,
        // so Cmd+arrow is the only way users reach either edge.
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("cmd-up", DocStart, ctx),
        KeyBinding::new("cmd-down", DocEnd, ctx),
        KeyBinding::new("shift-cmd-left", SelectHome, ctx),
        KeyBinding::new("shift-cmd-right", SelectEnd, ctx),
        KeyBinding::new("shift-cmd-up", SelectDocStart, ctx),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, ctx),
        // Line-edge deletion (Cmd+Delete on macOS).
        KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
        KeyBinding::new("cmd-delete", DeleteToLineEnd, ctx),
    ];
    for prefix in ["cmd", "ctrl"] {
        bindings.push(KeyBinding::new(&format!("{prefix}-z"), Undo, ctx));
        bindings.push(KeyBinding::new(&format!("shift-{prefix}-z"), Redo, ctx));
    }
    // Word-level editing: Option on macOS, Ctrl on Windows/Linux.
    let word_edit_prefix = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-left"),
        WordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-right"),
        WordRight,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        ctx,
    ));
    for prefix in ["cmd", "ctrl"] {
        bindings.push(KeyBinding::new(&format!("{prefix}-a"), SelectAll, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-c"), Copy, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-x"), Cut, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-v"), Paste, ctx));
    }
    // Palette-search context: TEXT-EDITING keys only. gpui dispatches matched
    // keybindings BEFORE raw key listeners (window.rs `dispatch_key_event`),
    // so anything bound here can never reach a palette's `on_key_down` —
    // navigation keys (up/down/left/right/enter) are deliberately unbound and
    // bubble to the palette frame instead.
    let palette = Some("PaletteSearch");
    let mut palette_bindings = vec![
        KeyBinding::new("backspace", Backspace, palette),
        KeyBinding::new("delete", Delete, palette),
        KeyBinding::new("home", Home, palette),
        KeyBinding::new("end", End, palette),
        KeyBinding::new("shift-left", SelectLeft, palette),
        KeyBinding::new("shift-right", SelectRight, palette),
        // Modifier-qualified motion is safe here: the palette's own navigation
        // uses BARE arrows/enter, which stay unbound and bubble to its frame.
        KeyBinding::new("cmd-left", Home, palette),
        KeyBinding::new("cmd-right", End, palette),
        KeyBinding::new("shift-cmd-left", SelectHome, palette),
        KeyBinding::new("shift-cmd-right", SelectEnd, palette),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, palette),
    ];
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-left"),
        WordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-right"),
        WordRight,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        palette,
    ));
    for prefix in ["cmd", "ctrl"] {
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-a"), SelectAll, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-c"), Copy, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-x"), Cut, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-v"), Paste, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-z"), Undo, palette));
        palette_bindings.push(KeyBinding::new(&format!("shift-{prefix}-z"), Redo, palette));
    }
    cx.bind_keys(palette_bindings);
    cx.bind_keys(bindings);
}

/// Events the input's owner (the composer, a settings page, a palette)
/// listens for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextInputEvent {
    Submitted,
    Edited,
    CursorMoved,
    ViewportChanged,
    MentionNavigate(isize),
    MentionAccept,
    MentionDismiss,
    /// Images pasted from the clipboard (screenshots / copied image data) —
    /// the wrapper stages them as attachments (use-attachments.ts onPaste).
    PastedImages(Vec<gpui::Image>),
    /// File paths pasted from the clipboard (a file manager "Copy").
    PastedPaths(Vec<PathBuf>),
}

/// Multiline input entity: content + selection + IME marked text + measured
/// layout (wrapped lines) for mouse mapping and auto-grow.
pub struct TextInput {
    /// Opted in only by real chat composers, never settings/search fields.
    pub use_chat_style: bool,
    /// Key context for the binding map ("Composer", or "PaletteSearch" for
    /// palette filters whose navigation keys must bubble).
    key_context: &'static str,
    pub focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    pub selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    is_selecting: bool,
    drag_position: Option<Point<Pixels>>,
    drag_generation: u64,
    drag_autoscroll_active: bool,
    /// Vertical scroll inside the input once content exceeds the max height.
    scroll_top: f32,
    /// Normally keeps the caret visible through edits and rewraps. Manual
    /// wheel scrolling pauses it until the next caret move or edit.
    follow_cursor: bool,
    // -- measured state (written during layout/paint) --
    last_lines: Vec<WrappedLine>,
    line_starts: Vec<usize>,
    last_bounds: Option<Bounds<Pixels>>,
    pub line_height: Pixels,
    pub content_height: f32,
    max_line_width: f32,
    pub last_width: f32,
    /// Raw Markdown → chip display projection from the last layout pass.
    projection: TextProjection,
    /// Inline completion preview: painted in faint ink after the text while
    /// the caret sits at the end (palette tab-completion). Owned by the
    /// wrapper — it recomputes and re-sets this on every render pass, so the
    /// input never has to know what the completion means.
    ghost: Option<SharedString>,
    /// Chips are the composer's feature (its mentions), not a behaviour of
    /// generic inputs (picker searches and rename fields also use this type).
    projector: Option<fn(&str) -> TextProjection>,
    /// Settings credentials only; never used for chat drafts.
    secret: bool,
    /// Bumped once per `layout_text` pass — the flip logic uses it to apply at
    /// most one compact↔expanded flip per layout (a flip is only re-evaluated
    /// after the input has been measured in the new mode).
    pub layout_epoch: u64,
    display_is_placeholder: bool,
    /// Caret blink anchor: reset on every keystroke/caret move so the caret is
    /// solid while typing and blinks at [`CARET_BLINK_MS`] when idle.
    blink_anchor: Instant,
    /// Half-period repaint driver, alive only while the input is focused.
    blink_task: Option<Task<()>>,
    // -- undo history --
    undo_stack: Vec<EditSnapshot>,
    redo_stack: Vec<EditSnapshot>,
    /// Kind, trailing offset, and time of the last edit — the merge test that
    /// decides whether the next edit extends the current undo step.
    last_edit: Option<(EditKind, usize, Instant)>,
    /// The owner's completion menu (the composer's mentions) keeps its own
    /// state; this only redirects bound keys while one of its tokens is
    /// active, keeping input focus and native text editing.
    mention_open: bool,
    mention_has_selection: bool,
    /// Last prepainted chip bounds; the paint-phase pointer listener uses
    /// these instead of attempting to infer text geometry from the cursor.
    chip_hits: Vec<ChipHit>,
    chip_tooltip: ChipTooltipPhase,
    chip_tooltip_generation: u64,
    chip_tooltip_popup: Option<Bounds<Pixels>>,
    chip_tooltip_task: Option<Task<()>>,
    /// Created once when Waiting promotes; retaining this entity preserves
    /// GPUI's global animation state across prepaint frames.
    chip_tooltip_view: Option<Entity<ChipTooltip>>,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self::with_context(placeholder, "Composer", cx)
    }

    /// An input in a custom KEY context — palettes use `"PaletteSearch"`,
    /// whose keymap binds only text-editing keys so navigation keys bubble to
    /// the surrounding frame (see `init`).
    pub fn with_context(
        placeholder: impl Into<SharedString>,
        key_context: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            use_chat_style: false,
            key_context,
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            is_selecting: false,
            drag_position: None,
            drag_generation: 0,
            drag_autoscroll_active: false,
            scroll_top: 0.0,
            follow_cursor: true,
            last_lines: Vec::new(),
            line_starts: vec![0],
            last_bounds: None,
            line_height: px(INPUT_LINE_HEIGHT),
            content_height: INPUT_LINE_HEIGHT,
            max_line_width: 0.0,
            last_width: 0.0,
            projection: TextProjection::default(),
            ghost: None,
            projector: None,
            secret: false,
            layout_epoch: 0,
            display_is_placeholder: true,
            blink_anchor: Instant::now(),
            blink_task: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            mention_open: false,
            mention_has_selection: false,
            chip_hits: Vec::new(),
            chip_tooltip: ChipTooltipPhase::Hidden,
            chip_tooltip_generation: 0,
            chip_tooltip_popup: None,
            chip_tooltip_task: None,
            chip_tooltip_view: None,
        }
    }

    /// Reset the caret blink phase (solid again) — called on every edit and
    /// caret move, matching textarea behavior.
    fn reset_blink(&mut self) {
        self.blink_anchor = Instant::now();
    }

    /// Caret paint gate: focused input in an active window, in the "on" blink
    /// phase. Also (re)arms the half-period repaint driver while focused, and
    /// drops it on blur so an unfocused input schedules no frames.
    fn caret_shown(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let focused = self.focus_handle.is_focused(window);
        if !focused || !window.is_window_active() {
            self.blink_task = None;
            return false;
        }
        if self.blink_task.is_none() {
            self.blink_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(CARET_BLINK_MS))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
        caret_visible(self.blink_anchor.elapsed().as_millis() as u64)
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    fn text_theme(&self, cx: &App) -> Theme {
        if self.use_chat_style {
            crate::appearance::chat_style::theme(cx)
        } else {
            Theme::of(cx).clone()
        }
    }

    pub fn settings_field(
        placeholder: impl Into<SharedString>,
        secret: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = Self::with_context(placeholder, "ProviderField", cx);
        input.secret = secret;
        input.refresh_projection();
        input
    }

    /// A settings field for multi-line prose (a subagent system prompt): the
    /// same native editing as [`Self::settings_field`], but Enter inserts a
    /// newline instead of submitting and the box grows to
    /// [`PROMPT_FIELD_MAX`] before scrolling.
    pub fn settings_prompt_field(
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = Self::with_context(placeholder, "PromptField", cx);
        input.refresh_projection();
        input
    }

    pub fn set_mention_controls(
        &mut self,
        open: bool,
        has_selection: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mention_open == open && self.mention_has_selection == has_selection {
            return;
        }
        self.mention_open = open;
        self.mention_has_selection = has_selection;
        cx.notify();
    }

    /// Show the text through `projector` from now on (the composer's
    /// mention chips).
    pub fn set_projector(&mut self, projector: fn(&str) -> TextProjection) {
        self.projector = Some(projector);
        self.refresh_projection();
    }

    fn refresh_projection(&mut self) {
        self.projection = if self.secret {
            TextProjection::secret(&self.content)
        } else if let Some(project) = self.projector {
            project(&self.content)
        } else {
            TextProjection::plain(&self.content)
        };
    }

    /// Replace a completed token (an `@query` or `#query` the owner's
    /// completion menu resolved) with the raw text of a chip — which the
    /// projection then shows as one — as one non-coalescing undo step.
    pub fn replace_with_chip(&mut self, range: Range<usize>, raw: String, cx: &mut Context<Self>) {
        self.invalidate_chip_tooltip();
        self.insert_raw(range, raw, cx);
    }

    /// The shared insertion path: splice `link` into the token range with a
    /// trailing space when none follows, one non-coalescing undo step, and
    /// the caret parked just past the inserted link.
    fn insert_raw(&mut self, range: Range<usize>, link: String, cx: &mut Context<Self>) {
        let next = self.content[range.end..].chars().next();
        let existing_separator = next.filter(|ch| ch.is_whitespace() && *ch != '\n' && *ch != '\r');
        let inserted = if existing_separator.is_some() {
            link
        } else {
            format!("{link} ")
        };
        self.record_edit(&range, &inserted);
        self.content =
            self.content[..range.start].to_owned() + &inserted + &self.content[range.end..];
        self.refresh_projection();
        let cursor =
            range.start + inserted.len() + existing_separator.map(char::len_utf8).unwrap_or(0);
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(TextInputEvent::Edited);
        cx.notify();
    }

    /// Replace a completed plain-text token (slash commands) as one
    /// non-coalescing undo step. Unlike [`Self::replace_with_chip`], the
    /// replacement is ordinary text — no link, no chip projection.
    pub fn replace_plain_token(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        cx: &mut Context<Self>,
    ) {
        self.insert_raw(range, replacement.to_owned(), cx);
    }

    /// Remove a typed plain-text token and the space after it as one undo
    /// step — a `/` menu action that types nothing, such as Attach files.
    pub fn remove_plain_token(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if range.start > range.end
            || range.end > self.content.len()
            || !self.content.is_char_boundary(range.start)
            || !self.content.is_char_boundary(range.end)
        {
            return;
        }
        let end = range.end + usize::from(self.content[range.end..].starts_with(' '));
        let range = range.start..end;
        self.record_edit(&range, "");
        self.content = self.content[..range.start].to_owned() + &self.content[range.end..];
        self.refresh_projection();
        self.selected_range = range.start..range.start;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(TextInputEvent::Edited);
        cx.notify();
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Set (or clear) the inline completion preview. Only paints while the
    /// caret sits at the end of a non-empty draft — see the prepaint gate.
    pub fn set_ghost(&mut self, ghost: Option<SharedString>, cx: &mut Context<Self>) {
        if self.ghost == ghost {
            return;
        }
        self.ghost = ghost;
        cx.notify();
    }

    pub fn has_newline(&self) -> bool {
        self.content.contains('\n')
    }

    /// Unwrapped width of the widest line — feeds the compact/expanded flip.
    pub fn measured_text_width(&self) -> f32 {
        self.max_line_width
    }

    pub fn measured_content_height(&self) -> f32 {
        self.content_height
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.invalidate_chip_tooltip();
        self.content = text.into();
        self.refresh_projection();
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_top = 0.0;
        self.follow_cursor = true;
        // Programmatic replacement (draft load, clear-on-submit) is a new
        // document, not an edit — undo must not reach back past it.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        self.reset_blink();
        cx.emit(TextInputEvent::Edited);
        cx.notify();
    }

    fn invalidate_chip_tooltip(&mut self) {
        self.chip_tooltip_generation = self.chip_tooltip_generation.wrapping_add(1);
        self.chip_tooltip = ChipTooltipPhase::Hidden;
        self.chip_tooltip_popup = None;
        self.chip_tooltip_task = None;
        self.chip_tooltip_view = None;
    }

    fn set_chip_hits(&mut self, hits: Vec<ChipHit>) {
        self.chip_hits = hits;
        let live = self
            .chip_tooltip
            .target()
            .is_none_or(|target| self.chip_hits.iter().any(|hit| &hit.target == target));
        if !live {
            self.invalidate_chip_tooltip();
        }
    }

    fn start_chip_tooltip_wait(&mut self, target: ChipTooltipTarget, cx: &mut Context<Self>) {
        self.chip_tooltip_generation = self.chip_tooltip_generation.wrapping_add(1);
        let generation = self.chip_tooltip_generation;
        self.chip_tooltip = ChipTooltipPhase::Waiting { target, generation };
        self.chip_tooltip_popup = None;
        self.chip_tooltip_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHIP_TOOLTIP_DELAY).await;
            this.update(cx, |input, cx| {
                let live = input
                    .chip_tooltip
                    .target()
                    .is_some_and(|target| input.chip_hits.iter().any(|hit| &hit.target == target));
                let next = chip_tooltip_promote(input.chip_tooltip.clone(), generation, live);
                if next != input.chip_tooltip {
                    input.chip_tooltip = next;
                    input.chip_tooltip_task = None;
                    if let ChipTooltipPhase::Visible { target, generation } = &input.chip_tooltip {
                        input.chip_tooltip_view = Some(cx.new(|_| ChipTooltip {
                            label: target.label.clone(),
                            activation: *generation,
                        }));
                    }
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn on_chip_pointer_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.invalidate_chip_tooltip();
            return;
        }
        let target = self
            .chip_hits
            .iter()
            .find(|hit| hit.bounds.contains(&position))
            .map(|hit| hit.target.clone());
        let in_popup = self
            .chip_tooltip_popup
            .is_some_and(|popup| popup.contains(&position));
        let next_generation = self.chip_tooltip_generation.wrapping_add(1);
        let next = chip_tooltip_reduce(
            self.chip_tooltip.clone(),
            target.clone(),
            in_popup,
            next_generation,
        );
        if next == self.chip_tooltip {
            return;
        }
        match next {
            ChipTooltipPhase::Waiting { target, .. } => self.start_chip_tooltip_wait(target, cx),
            _ => {
                self.invalidate_chip_tooltip();
                self.chip_tooltip = next;
                cx.notify();
            }
        }
    }

    fn visible_chip_tooltip(
        &self,
    ) -> Option<(ChipTooltipTarget, Point<Pixels>, u64, Entity<ChipTooltip>)> {
        let ChipTooltipPhase::Visible { target, generation } = &self.chip_tooltip else {
            return None;
        };
        self.chip_hits
            .iter()
            .find(|hit| hit.target == *target)
            .and_then(|hit| {
                let view = self.chip_tooltip_view.clone()?;
                Some((target.clone(), hit.anchor, *generation, view))
            })
    }

    fn check_chip_tooltip_visibility(
        &mut self,
        popup: Bounds<Pixels>,
        pointer: Point<Pixels>,
    ) -> bool {
        let Some((target, _, _, _)) = self.visible_chip_tooltip() else {
            return false;
        };
        let in_chip = self
            .chip_hits
            .iter()
            .any(|hit| hit.target == target && hit.bounds.contains(&pointer));
        if chip_tooltip_contains(in_chip, popup.contains(&pointer)) {
            self.chip_tooltip_popup = Some(popup);
            true
        } else {
            self.invalidate_chip_tooltip();
            false
        }
    }
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// UTF-16 offset → byte offset, measured *within `text`*.
///
/// The two are interchangeable only while the text stays in the BMP's
/// single-byte range; every CJK character widens the byte offset by two past
/// the UTF-16 one, so the string the offset was expressed against is the one it
/// has to be resolved against.
pub(super) fn utf16_to_byte_offset(text: &str, offset: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;
    for ch in text.chars() {
        if utf16_count >= offset {
            break;
        }
        utf16_count += ch.len_utf16();
        utf8_offset += ch.len_utf8();
    }
    utf8_offset
}

/// The custom element: measured auto-grow layout + shaped-line painting.
struct TextInputElement {
    input: Entity<TextInput>,
    /// Max content height before internal scrolling kicks in.
    max_content_height: f32,
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.text_theme(cx);
        let (font_size, line_height) = if self.use_chat_style {
            let style = crate::appearance::chat_style::settings(cx);
            (style.font_size, style.input_line_height())
        } else {
            (INPUT_TEXT_SIZE, INPUT_LINE_HEIGHT)
        };
        let text_color = if self.content.is_empty() {
            theme.text_faint
        } else {
            theme.text
        };
        div()
            .key_context(self.key_context)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::mention_tab))
            .on_action(cx.listener(Self::mention_escape))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .w_full()
            .text_size(px(font_size))
            .line_height(px(line_height))
            .text_color(text_color)
            .font_family(theme.font_sans.clone())
            .child(TextInputElement {
                input: cx.entity(),
                // Internal scrolling once content exceeds the 260px textarea
                // box minus its `pt-4 pb-1` padding.
                max_content_height: match self.key_context {
                    "ProviderField" => INPUT_LINE_HEIGHT,
                    "PromptField" => PROMPT_FIELD_MAX,
                    _ => TEXTAREA_MAX - TEXTAREA_PAD_V,
                },
            })
    }
}

#[cfg(test)]
mod tests;
