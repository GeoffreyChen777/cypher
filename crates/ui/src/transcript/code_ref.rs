//! File-reference chips in replies ([`crate::markdown::code_ref`]): the
//! preview card a hovered chip opens and the click that opens the file.
//!
//! The card shows the cited lines with a little context, syntax colored, as
//! the file is in the session's checkout NOW — a reply can be older than
//! the code, so the card says so rather than claim it is what the agent saw.
//! It stays up while the pointer is on the chip or on the card (crossing
//! the gap between them is forgiven), so its code can be scrolled, selected
//! and copied.
//!
//! Agents write paths relative to wherever they were looking: the checkout
//! root (`crates/ui/src/a.rs`), a package inside it (`core/context.py`), or
//! just a name once the full path was given (`file.py:128`). A reference is
//! resolved by reading it at the root first, then by a file search for
//! paths ending in it; a bare name is first expanded to the one longer path
//! of that name elsewhere in the same reply. Several candidates are listed,
//! never guessed between.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cypher_proto::{FileSearchMatch, WorkspaceFileContent};
use cypher_rpc::{RpcError, methods};
use cypher_syntax::HighlightedDocument;
use gpui::{
    AnyElement, App, AsyncWindowContext, ClipboardItem, Context, SharedString, Task, WeakEntity,
    Window, div, point, prelude::*, px,
};

use crate::files::{FilesContext, LinkContexts, links, roots};
use crate::kit::icons::{self, icon};
use crate::kit::motion;
use crate::kit::popover;
use crate::kit::theme::{MonoStyled, Theme};
use crate::markdown::code_ref::{self, CodeRef};
use crate::markdown::render::{self, CodeRefHover, CodeRefUi, RenderOptions};
use crate::markdown::selection::{self, SelectionScope};
use crate::state::EngineHandle;

use super::{RowKind, Transcript};

/// Hover this long before a chip opens its card (a pointer passing over
/// a paragraph must not flash cards).
const OPEN_DELAY: Duration = Duration::from_millis(350);
/// Leaving both the chip and the card closes it after this grace — long
/// enough to cross the gap between them.
const CLOSE_DELAY: Duration = Duration::from_millis(220);
/// Lines shown around the cited ones.
const CONTEXT_LINES: usize = 3;
/// A whole-file reference previews the file's head.
const HEAD_LINES: usize = 40;
/// An excerpt never holds more lines than this; a longer range is cut.
const MAX_EXCERPT_LINES: usize = 200;
/// A preview read stays fresh this long; showing it later reads again.
const FRESH_FOR: Duration = Duration::from_secs(15);
/// Larger files preview without syntax colors.
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
const CARD_WIDTH: f32 = 580.0;
/// The body scrolls past this many lines.
const BODY_MAX_LINES: f32 = 16.0;
const BODY_PAD_Y: f32 = 6.0;
const HEADER_HEIGHT: f32 = 34.0;
const FOOTER_HEIGHT: f32 = 24.0;
/// The card's entrance: it fades in, settles [`ENTRANCE_RISE`] px out of
/// the chip and its frost thickens with it.
const ENTRANCE: motion::MotionSpec = motion::MotionSpec::new(180, motion::EASE_OUT_EXPO);
const ENTRANCE_RISE: f32 = 6.0;

/// The card's state, owned by the transcript.
#[derive(Default)]
pub(super) struct CodeRefPreview {
    /// The chip the card is for, shown or waiting out [`OPEN_DELAY`].
    target: Option<Target>,
    shown: bool,
    /// When the card last appeared — the clock of its entrance. A card
    /// following the pointer to the next chip keeps it (no replay).
    shown_at: Option<Instant>,
    on_chip: bool,
    on_card: bool,
    /// The pending open or close.
    timer: Option<Task<()>>,
    /// A click that came before its reference resolved: open it then.
    open_when_resolved: Option<(ResolveKey, Option<(usize, usize)>)>,
    /// The card's Copy feedback, for its current target.
    copied: bool,
    resolutions: HashMap<ResolveKey, Resolved>,
    files: HashMap<FileKey, FileSlot>,
    /// The card's own text-selection scope (its code selects and copies).
    scope: Option<SelectionScope>,
    ui: Option<CodeRefUi>,
}

#[derive(Clone)]
struct Target {
    hover: CodeRefHover,
    key: ResolveKey,
}

/// A reference to resolve: the chat, and the path to look for — as
/// written, or its expansion from the same reply.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ResolveKey {
    chat_id: String,
    path: String,
}

struct Resolved {
    resolution: Resolution,
    at: Instant,
    _task: Option<Task<()>>,
}

#[derive(Clone, Debug, PartialEq)]
enum Resolution {
    Pending,
    /// In the checkout, at this path relative to it.
    Found(String),
    Missing,
    /// Several files end in the path (the card lists them).
    Ambiguous(Vec<String>),
    /// An absolute path outside the checkout.
    Outside(String),
    Failed(SharedString),
}

/// `(chat id, path relative to the checkout)`.
type FileKey = (String, String);

struct FileSlot {
    state: FileState,
    read_at: Instant,
    task: Option<Task<()>>,
}

enum FileState {
    Loading,
    Ready(Arc<FileSnapshot>),
    Failed(SharedString),
}

/// One read of a file, split into lines and (when small enough) colored.
struct FileSnapshot {
    text: String,
    binary: bool,
    truncated: bool,
    line_starts: Vec<usize>,
    highlight: Option<Arc<HighlightedDocument>>,
}

impl FileSnapshot {
    fn new(path: &str, content: WorkspaceFileContent) -> Self {
        let text = content.text.unwrap_or_default();
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .filter(|&start| start < text.len() || start == 0)
            .collect();
        let highlight = (!content.binary && text.len() <= MAX_HIGHLIGHT_BYTES)
            .then(|| {
                cypher_syntax::highlight(cypher_syntax::HighlightRequest {
                    source: &text,
                    path: Some(path),
                    fence_tag: None,
                })
                .ok()
            })
            .flatten()
            .map(Arc::new);
        Self {
            text,
            binary: content.binary,
            truncated: content.truncated,
            line_starts,
            highlight,
        }
    }

    fn line_count(&self) -> usize {
        self.line_starts.len()
    }
}

/// The lines a card shows for a reference.
#[derive(Debug, PartialEq)]
struct Excerpt {
    /// 1-based number of the excerpt's first line.
    first: usize,
    /// 1-based number of its last line.
    last: usize,
    /// The cited lines within it, 0-based from the excerpt's start.
    band: Option<(usize, usize)>,
}

/// Which lines of a `total`-line file preview `lines`: the cited ones with
/// [`CONTEXT_LINES`] around them, or the head for a whole file. `None` when
/// the cited lines start past the end.
fn excerpt(total: usize, lines: Option<(usize, usize)>) -> Option<Excerpt> {
    let total = total.max(1);
    match lines {
        Some((cited_first, cited_last)) => {
            if cited_first > total {
                return None;
            }
            let cited_last = cited_last.clamp(cited_first, total);
            let first = cited_first.saturating_sub(CONTEXT_LINES).max(1);
            let last = (cited_last + CONTEXT_LINES)
                .min(total)
                .min(first + MAX_EXCERPT_LINES - 1);
            Some(Excerpt {
                first,
                last,
                band: Some((cited_first - first, cited_last.min(last) - first)),
            })
        }
        None => Some(Excerpt {
            first: 1,
            last: total.min(HEAD_LINES),
            band: None,
        }),
    }
}

/// A checkout path as a link the shell's file-link handler reads back:
/// the characters a link would cut at are escaped.
pub(super) fn file_url(path: &str, lines: Option<(usize, usize)>) -> String {
    let mut url = String::with_capacity(path.len() + 12);
    for c in path.chars() {
        match c {
            '%' => url.push_str("%25"),
            '#' => url.push_str("%23"),
            '?' => url.push_str("%3F"),
            c => url.push(c),
        }
    }
    match lines {
        Some((first, last)) if last > first => url.push_str(&format!(":{first}-{last}")),
        Some((first, _)) => url.push_str(&format!(":{first}")),
        None => {}
    }
    url
}

impl Transcript {
    /// The chip wiring every reply row renders with (one instance, cloned).
    pub(super) fn code_ref_ui(&mut self, cx: &mut Context<Self>) -> CodeRefUi {
        if let Some(ui) = &self.code_refs.ui {
            return ui.clone();
        }
        let (hover, click) = (cx.weak_entity(), cx.weak_entity());
        let ui = CodeRefUi {
            on_hover: Rc::new(move |chip, window, cx| {
                hover
                    .update(cx, |this, cx| this.code_ref_hovered(chip, window, cx))
                    .ok();
            }),
            on_click: Rc::new(move |chip, window, cx| {
                click
                    .update(cx, |this, cx| this.code_ref_clicked(chip, window, cx))
                    .ok();
            }),
        };
        self.code_refs.ui = Some(ui.clone());
        ui
    }

    fn code_ref_hovered(
        &mut self,
        chip: Option<CodeRefHover>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The chip's hover wash is paint the transcript's cached view only
        // redraws when notified — on leave as much as on enter.
        cx.notify();
        let Some(chip) = chip else {
            self.code_refs.on_chip = false;
            if self.code_refs.shown {
                self.schedule_code_ref_close(cx);
            } else {
                self.code_refs.target = None;
                self.code_refs.timer = None;
            }
            return;
        };
        self.code_refs.on_chip = true;
        if let Some(target) = &mut self.code_refs.target
            && target.hover.key == chip.key
        {
            // Back on the chip the card is for: keep it.
            target.hover.anchor = chip.anchor;
            if self.code_refs.shown {
                self.code_refs.timer = None;
            }
            return;
        }
        let key = self.code_ref_key(&chip, cx);
        self.code_refs.target = Some(Target { hover: chip, key });
        self.code_refs.copied = false;
        if self.code_refs.shown {
            // A card is up: it follows the pointer to the next chip at once.
            self.code_refs.timer = None;
            self.ensure_code_ref(window, cx);
            cx.notify();
            return;
        }
        self.code_refs.timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(OPEN_DELAY).await;
            this.update_in(cx, |this, window, cx| {
                this.code_refs.timer = None;
                if this.code_refs.target.is_some() && this.code_refs.on_chip {
                    this.code_refs.shown = true;
                    this.code_refs.shown_at = Some(Instant::now());
                    this.ensure_code_ref(window, cx);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn code_ref_card_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.code_refs.on_card = hovered;
        if hovered {
            self.code_refs.timer = None;
        } else if self.code_refs.shown {
            self.schedule_code_ref_close(cx);
        }
    }

    fn schedule_code_ref_close(&mut self, cx: &mut Context<Self>) {
        self.code_refs.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CLOSE_DELAY).await;
            this.update(cx, |this, cx| {
                this.code_refs.timer = None;
                if !this.code_refs.on_chip && !this.code_refs.on_card {
                    this.hide_code_ref(cx);
                }
            })
            .ok();
        }));
    }

    /// Close the card (a scroll moved the chip, a chip was clicked, the
    /// pointer left). The chip's hover is forgotten too, so a pointer still
    /// over a chip opens it again on its next move.
    pub(super) fn hide_code_ref(&mut self, cx: &mut Context<Self>) {
        let preview = &mut self.code_refs;
        if preview.target.is_none() && !preview.shown {
            return;
        }
        preview.target = None;
        preview.shown = false;
        preview.shown_at = None;
        preview.on_chip = false;
        preview.on_card = false;
        preview.timer = None;
        if let Some(scope) = preview.scope {
            selection::clear(scope);
        }
        render::clear_chip_hover(self.scope, crate::kit::motion::reduced_motion(cx));
        cx.notify();
    }

    fn code_ref_clicked(
        &mut self,
        chip: CodeRefHover,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines = chip.code_ref.lines;
        let key = self.code_ref_key(&chip, cx);
        match self.resolve_code_ref(&key, window, cx) {
            Resolution::Found(path) => {
                self.hide_code_ref(cx);
                self.open_code_ref(path, lines, window, cx);
            }
            Resolution::Pending => {
                self.hide_code_ref(cx);
                self.code_refs.open_when_resolved = Some((key, lines));
            }
            // Nothing to open: the card says why.
            _ => {
                self.code_refs.target = Some(Target { hover: chip, key });
                if !self.code_refs.shown {
                    self.code_refs.shown_at = Some(Instant::now());
                }
                self.code_refs.shown = true;
                self.code_refs.timer = None;
                cx.notify();
            }
        }
    }

    /// Open a checkout file at `lines` in this tile's Files tab — deferred
    /// past the current update, as the shell's handler reaches into other
    /// entities.
    fn open_code_ref(
        &self,
        path: String,
        lines: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(handler) = self.link_handler.clone() else {
            return;
        };
        let url = file_url(&path, lines);
        window.defer(cx, move |window, cx| {
            handler(&url, window, cx);
        });
    }

    /// What to resolve a chip as: the bare name expanded from its reply
    /// when the reply names exactly one longer path for it.
    fn code_ref_key(&self, chip: &CodeRefHover, cx: &App) -> ResolveKey {
        let chat_id = self
            .state
            .read(cx)
            .selected_chat_row()
            .map(|chat| chat.id.clone())
            .unwrap_or_default();
        let tree = self
            .rows
            .iter()
            .find(|row| row.id == chip.row_key)
            .and_then(|row| match &row.kind {
                RowKind::Markdown { tree, .. }
                | RowKind::LiveMarkdown { tree, .. }
                | RowKind::ThoughtBlock { tree, .. } => Some(tree.clone()),
                _ => None,
            });
        let path = tree
            .and_then(|tree| code_ref::expand_with(&chip.code_ref, &code_ref::refs_in(&tree)))
            .unwrap_or_else(|| chip.code_ref.path.clone());
        ResolveKey { chat_id, path }
    }

    /// Kick what the shown card needs: its reference's resolution, then the
    /// file's text (again, when the last read is older than [`FRESH_FOR`]).
    fn ensure_code_ref(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.code_refs.target.clone() else {
            return;
        };
        if let Resolution::Found(path) = self.resolve_code_ref(&target.key, window, cx) {
            self.load_code_ref_file(&target.key.chat_id, path, cx);
        }
    }

    /// The reference's resolution, starting it (or redoing a stale miss).
    fn resolve_code_ref(
        &mut self,
        key: &ResolveKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Resolution {
        if let Some(resolved) = self.code_refs.resolutions.get(key) {
            let settled_miss = !matches!(
                resolved.resolution,
                Resolution::Pending | Resolution::Found(_)
            );
            if !(settled_miss && resolved.at.elapsed() > FRESH_FOR) {
                return resolved.resolution.clone();
            }
        }
        let contexts = crate::files::link_contexts(self.state.read(cx));
        let engine = self.state.read(cx).engine().cloned();
        let (contexts, engine) = match (contexts, engine) {
            (Ok(contexts), Some(engine))
                if contexts
                    .contexts
                    .first()
                    .is_some_and(|context| context.chat_id == key.chat_id) =>
            {
                (contexts, engine)
            }
            (Err(message), _) => {
                let resolution =
                    self.settle_resolution(key.clone(), Resolution::Failed(message.into()));
                // A workspace chat's roots still on their way: the next
                // hover asks again rather than keep the miss.
                if self.state.read(cx).file_roots() == crate::state::FileRoots::Loading
                    && let Some(resolved) = self.code_refs.resolutions.get_mut(key)
                    && let Some(stale) = Instant::now().checked_sub(FRESH_FOR * 2)
                {
                    resolved.at = stale;
                }
                return resolution;
            }
            _ => {
                return self.settle_resolution(
                    key.clone(),
                    Resolution::Failed("Not connected to the session's device.".into()),
                );
            }
        };
        let task_key = key.clone();
        let home = super::images::chat_home(self.state.read(cx));
        let task = cx.spawn_in(window, async move |this, cx| {
            let (resolution, read) =
                resolve(&engine, &contexts, &task_key.path, home.as_deref(), cx).await;
            finish_resolution(this, task_key, resolution, read, cx);
        });
        self.code_refs.resolutions.insert(
            key.clone(),
            Resolved {
                resolution: Resolution::Pending,
                at: Instant::now(),
                _task: Some(task),
            },
        );
        Resolution::Pending
    }

    fn settle_resolution(&mut self, key: ResolveKey, resolution: Resolution) -> Resolution {
        self.code_refs.resolutions.insert(
            key,
            Resolved {
                resolution: resolution.clone(),
                at: Instant::now(),
                _task: None,
            },
        );
        resolution
    }

    fn load_code_ref_file(&mut self, chat_id: &str, path: String, cx: &mut Context<Self>) {
        let file_key = (chat_id.to_owned(), path.clone());
        if let Some(slot) = self.code_refs.files.get(&file_key)
            && (slot.task.is_some() || slot.read_at.elapsed() < FRESH_FOR)
        {
            return;
        }
        let Ok(contexts) = crate::files::link_contexts(self.state.read(cx)) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some((context, relative, _)) = locate(&contexts, &path) else {
            return;
        };
        if context.chat_id != chat_id {
            return;
        }
        let task_key = file_key.clone();
        let task = cx.spawn(async move |this, cx| {
            let read = read_file(&engine, &context, &relative, cx).await;
            this.update(cx, |this, cx| {
                this.store_code_ref_file(task_key, read);
                cx.notify();
            })
            .ok();
        });
        // A refresh keeps showing the previous read until the new one lands.
        let slot = self
            .code_refs
            .files
            .entry(file_key)
            .or_insert_with(|| FileSlot {
                state: FileState::Loading,
                read_at: Instant::now(),
                task: None,
            });
        slot.task = Some(task);
    }

    fn store_code_ref_file(&mut self, key: FileKey, read: Result<FileSnapshot, SharedString>) {
        let state = match read {
            Ok(snapshot) => FileState::Ready(Arc::new(snapshot)),
            Err(message) => FileState::Failed(message),
        };
        // Previews stay few: drop the stalest reads past a small budget.
        if self.code_refs.files.len() >= 24 && !self.code_refs.files.contains_key(&key) {
            let stalest = self
                .code_refs
                .files
                .iter()
                .filter(|(_, slot)| slot.task.is_none())
                .min_by_key(|(_, slot)| slot.read_at)
                .map(|(key, _)| key.clone());
            if let Some(stalest) = stalest {
                self.code_refs.files.remove(&stalest);
            }
        }
        self.code_refs.files.insert(
            key,
            FileSlot {
                state,
                read_at: Instant::now(),
                task: None,
            },
        );
    }

    /// The card, mounted on the floating layer under (or over) its chip.
    pub(super) fn code_ref_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.code_refs.shown {
            return None;
        }
        let target = self.code_refs.target.clone()?;
        let theme = crate::appearance::chat_style::theme(cx);
        let scope = *self
            .code_refs
            .scope
            .get_or_insert_with(selection::next_preview_scope);
        let resolution = self
            .code_refs
            .resolutions
            .get(&target.key)
            .map_or(Resolution::Pending, |resolved| resolved.resolution.clone());
        let file = match &resolution {
            Resolution::Found(path) => self
                .code_refs
                .files
                .get(&(target.key.chat_id.clone(), path.clone()))
                .map(|slot| match &slot.state {
                    FileState::Loading => Err(None),
                    FileState::Ready(snapshot) => Ok(snapshot.clone()),
                    FileState::Failed(message) => Err(Some(message.clone())),
                }),
            _ => None,
        };
        let code_ref = &target.hover.code_ref;
        let lines = code_ref.lines;
        let line_height = theme.markdown.code_line_height;
        let viewport = window.viewport_size();
        let width = CARD_WIDTH.min(f32::from(viewport.width) - 16.0).max(240.0);

        // An image file previews as its picture, not its bytes.
        let picture = match &resolution {
            Resolution::Found(path)
                if crate::markdown::image_source::Format::for_path(path).is_some() =>
            {
                self.code_ref_picture(&target.key.chat_id, path, width, &theme, cx)
            }
            _ => None,
        };
        let (body, body_lines) = match (&resolution, &file) {
            _ if picture.is_some() => picture.unwrap_or_else(|| (div().into_any_element(), 0.0)),
            (Resolution::Found(_), Some(Ok(snapshot))) => {
                self.code_ref_excerpt(snapshot, lines, scope, &theme)
            }
            (Resolution::Found(_), Some(Err(Some(message)))) | (Resolution::Failed(message), _) => {
                (card_note(&theme, message.clone(), None), 2.0)
            }
            (Resolution::Pending, _) | (Resolution::Found(_), _) => {
                (card_note(&theme, "Loading…".into(), None), 2.0)
            }
            (Resolution::Missing, _) => (
                card_note(
                    &theme,
                    "Not found in this project.".into(),
                    Some(target.key.path.clone().into()),
                ),
                2.0,
            ),
            (Resolution::Outside(path), _) => (
                card_note(
                    &theme,
                    "Outside this project — not previewed.".into(),
                    Some(path.clone().into()),
                ),
                2.0,
            ),
            (Resolution::Ambiguous(paths), _) => {
                let count = paths.len() as f32;
                (self.code_ref_choices(paths, lines, &theme, cx), count + 1.5)
            }
        };

        let open_path = match &resolution {
            Resolution::Found(path) => path.clone(),
            _ => target.key.path.clone(),
        };
        let shown_path = self.code_ref_shown(&open_path, cx);
        let header =
            self.code_ref_header(&shown_path, &open_path, code_ref, &resolution, &theme, cx);
        let footer = div()
            .h(px(FOOTER_HEIGHT))
            .px(px(10.0))
            .flex()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .child(match &resolution {
                Resolution::Found(_) => "Current project version · click the reference to open",
                _ => "Project lookup",
            });
        let card = popover::popover_card_flush(&theme)
            // Frosted on every theme: a translucent tint over the blurred
            // reply, so the card reads as floating over the text it explains.
            .bg(card_tint(&theme))
            .id("code-ref-card")
            .w(px(width))
            .flex()
            .flex_col()
            .occlude()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.code_ref_card_hovered(*hovered, cx);
            }))
            // First child: clears the card's selection registry each frame.
            .child(render::selection_frame_reset(scope))
            .child(header)
            .child(body)
            .child(footer);
        let card = render::shield_selection(card);
        // The entrance runs off the wall clock rather than `with_animation`:
        // the backdrop blur ignores element opacity, so it has to grow with
        // the fade or a full-strength frosted rectangle would pop in first.
        let t = match self.code_refs.shown_at {
            Some(since) if !motion::reduced_motion(cx) => {
                let raw = since.elapsed().as_secs_f32() / ENTRANCE.total().as_secs_f32();
                if raw < 1.0 {
                    window.request_animation_frame();
                    ENTRANCE.progress(raw)
                } else {
                    1.0
                }
            }
            _ => 1.0,
        };
        let card = crate::kit::frost::frosted_always(12.0, crate::kit::frost::MENU_BLUR * t, card);

        // Below the chip when it fits (or fits better), else above.
        let anchor = target.hover.anchor;
        let height = HEADER_HEIGHT
            + FOOTER_HEIGHT
            + (body_lines.min(BODY_MAX_LINES) * line_height + 2.0 * BODY_PAD_Y);
        let below = f32::from(viewport.height - anchor.bottom()) - 12.0;
        let above = f32::from(anchor.top()) - 12.0;
        let (position, corner, toward_chip) = if below >= height || below >= above {
            (
                point(anchor.left(), anchor.bottom() + px(4.0)),
                gpui::Anchor::TopLeft,
                -1.0,
            )
        } else {
            (
                point(anchor.left(), anchor.top() - px(4.0)),
                gpui::Anchor::BottomLeft,
                1.0,
            )
        };
        // It starts tucked toward the chip and slides out of it.
        let card = div()
            .relative()
            .top(px(toward_chip * ENTRANCE_RISE * (1.0 - t)))
            .opacity(t)
            .child(card);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(position)
                    .anchor(corner)
                    .snap_to_window_with_margin(px(8.0))
                    .child(card),
            )
            .priority(2)
            .into_any_element(),
        )
    }

    /// How a resolved path reads on the card: as it is for a session; with
    /// its project's name first in a workspace's own chat.
    fn code_ref_shown(&self, path: &str, cx: &App) -> String {
        match crate::files::link_contexts(self.state.read(cx)) {
            Ok(contexts) if contexts.multi_root() => locate(&contexts, path)
                .map(|(_, _, shown)| shown)
                .unwrap_or_else(|| path.to_owned()),
            _ => path.to_owned(),
        }
    }

    /// `path` is what the card shows, `open_path` what Copy and Open use.
    fn code_ref_header(
        &mut self,
        path: &str,
        open_path: &str,
        code_ref: &CodeRef,
        resolution: &Resolution,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (format!("{dir}/"), name.to_owned()),
            None => (String::new(), path.to_owned()),
        };
        let lines_label = match code_ref.lines {
            Some((first, last)) if last > first => format!("L{first}–{last}"),
            Some((first, _)) => format!("L{first}"),
            None => String::new(),
        };
        let reference = format!("{open_path}{}", code_ref.line_suffix());
        let found = matches!(resolution, Resolution::Found(_));
        let copied = self.code_refs.copied;
        div()
            .h(px(HEADER_HEIGHT))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                icon(icons::DOCUMENT)
                    .flex_none()
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            )
            // The path reads from its end: the name stays, a long folder
            // part gives way.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .overflow_hidden()
                    .mono(theme)
                    .text_size(px(12.0))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink(1.0)
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(theme.text_faint)
                            .child(dir),
                    )
                    .child(div().flex_none().text_color(theme.text).child(name))
                    .when(!lines_label.is_empty(), |el| {
                        el.child(
                            div()
                                .flex_none()
                                .ml(px(8.0))
                                .text_color(theme.text_muted)
                                .child(lines_label),
                        )
                    }),
            )
            .child(
                card_button(theme, "code-ref-copy")
                    .child(
                        icon(if copied { icons::CHECK } else { icons::COPY })
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    )
                    .child(if copied { "Copied" } else { "Copy" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(reference.clone()));
                        this.code_refs.copied = true;
                        cx.notify();
                    })),
            )
            .when(found && self.link_handler.is_some(), |el| {
                let path = open_path.to_owned();
                let lines = code_ref.lines;
                el.child(
                    card_button(theme, "code-ref-open")
                        .child("Open")
                        .child(
                            icon(icons::ARROW_RIGHT)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.hide_code_ref(cx);
                            this.open_code_ref(path.clone(), lines, window, cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// The excerpt body and its line count (for placement).
    fn code_ref_excerpt(
        &self,
        snapshot: &FileSnapshot,
        lines: Option<(usize, usize)>,
        scope: SelectionScope,
        theme: &Theme,
    ) -> (AnyElement, f32) {
        if snapshot.binary {
            return (
                card_note(theme, "Binary file — no preview.".into(), None),
                2.0,
            );
        }
        let Some(excerpt) = excerpt(snapshot.line_count(), lines) else {
            let first = lines.map_or(0, |(first, _)| first);
            let message = format!(
                "Line {first} is past the end of the file ({} lines{}).",
                snapshot.line_count(),
                if snapshot.truncated {
                    ", preview truncated"
                } else {
                    ""
                }
            );
            return (card_note(theme, message.into(), None), 2.0);
        };
        let start = snapshot.line_starts[excerpt.first - 1];
        let end = snapshot
            .line_starts
            .get(excerpt.last)
            .map_or(snapshot.text.len(), |next| next - 1);
        let mut code = snapshot.text[start..end.max(start)].replace('\r', "");
        // The file's final newline ends its last line; it is not a line.
        if code.ends_with('\n') {
            code.pop();
        }
        let highlight = snapshot
            .highlight
            .as_ref()
            .and_then(|document| document.lines.get(excerpt.first - 1..excerpt.last));
        let line_height = theme.markdown.code_line_height;
        let size = theme.markdown.code_size;
        let count = excerpt.last - excerpt.first + 1;
        let digits = excerpt.last.to_string().len() as f32;

        let mut opts = RenderOptions::settled("code-ref-card".into());
        opts.scope = scope;
        let code_el =
            render::code_excerpt(&code, highlight, &opts, theme, "code-ref-card-x".into())
                .flex_1()
                .min_w_0()
                .pr(px(12.0));
        let band = excerpt.band;
        let gutter = div()
            .flex_none()
            .w(px(digits * size * 0.62 + 22.0))
            .pr(px(12.0))
            .flex()
            .flex_col()
            .items_end()
            .mono(theme)
            .children((excerpt.first..=excerpt.last).map(|number| {
                let ix = number - excerpt.first;
                let cited = band.is_some_and(|(a, b)| ix >= a && ix <= b);
                div()
                    .text_color(if cited {
                        theme.text_muted
                    } else {
                        theme.text_faint.opacity(0.7)
                    })
                    .child(SharedString::from(number.to_string()))
            }));
        let body = div()
            .id("code-ref-card-body")
            .max_h(px(BODY_MAX_LINES * line_height + 2.0 * BODY_PAD_Y))
            .overflow_y_scroll()
            .bg(crate::kit::theme::code_block_background(theme))
            .child(
                div()
                    .relative()
                    .py(px(BODY_PAD_Y))
                    .flex()
                    .flex_row()
                    .text_size(px(size))
                    .line_height(px(line_height))
                    .when_some(band, |el, (a, b)| {
                        let top = BODY_PAD_Y + a as f32 * line_height;
                        let height = (b - a + 1) as f32 * line_height;
                        el.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(px(top))
                                .h(px(height))
                                .bg(theme.accent.opacity(0.10)),
                        )
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(top))
                                .h(px(height))
                                .w(px(2.0))
                                .bg(theme.accent.opacity(0.7)),
                        )
                    })
                    .child(gutter)
                    .child(code_el),
            )
            .into_any_element();
        (body, count as f32)
    }

    /// Several files end in the reference's path: list them to open.
    fn code_ref_choices(
        &self,
        paths: &[String],
        lines: Option<(usize, usize)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let can_open = self.link_handler.is_some();
        div()
            .p(px(6.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .px(px(6.0))
                    .py(px(2.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(format!("{} files match — which one?", paths.len())),
            )
            .children(paths.iter().enumerate().map(|(ix, path)| {
                let open = path.clone();
                let shown = self.code_ref_shown(path, cx);
                div()
                    .id(("code-ref-choice", ix))
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(5.0))
                    .mono(theme)
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .when(can_open, |el| {
                        el.cursor_pointer()
                            .hover(|el| el.bg(theme.ink(0.06)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.hide_code_ref(cx);
                                this.open_code_ref(open.clone(), lines, window, cx);
                            }))
                    })
                    .child(shown)
            }))
            .into_any_element()
    }
}

/// Store a finished resolution (and the read it made), then open the file
/// if a click was waiting on it.
fn finish_resolution(
    this: WeakEntity<Transcript>,
    key: ResolveKey,
    resolution: Resolution,
    read: Option<(String, Result<FileSnapshot, SharedString>)>,
    cx: &mut AsyncWindowContext,
) {
    this.update_in(cx, |this, window, cx| {
        this.settle_resolution(key.clone(), resolution.clone());
        if let Some((path, read)) = read {
            this.store_code_ref_file((key.chat_id.clone(), path), read);
        }
        if let Some((waiting, lines)) = this.code_refs.open_when_resolved.take() {
            match &resolution {
                Resolution::Found(path) if waiting == key => {
                    this.open_code_ref(path.clone(), lines, window, cx);
                }
                _ if waiting != key => this.code_refs.open_when_resolved = Some((waiting, lines)),
                _ => {}
            }
        }
        if this
            .code_refs
            .target
            .as_ref()
            .is_some_and(|target| target.key == key)
        {
            this.ensure_code_ref(window, cx);
        }
        cx.notify();
    })
    .ok();
}

/// Where an open path (a resolution's [`Resolution::Found`]) is read: the
/// context of its root, the path in that root, and how it reads to the
/// owner. A session's paths are its checkout's; a workspace chat's are
/// absolute, inside one of its roots.
fn locate(contexts: &LinkContexts, path: &str) -> Option<(FilesContext, String, String)> {
    if !contexts.multi_root() {
        let context = contexts.contexts.first()?.clone();
        return Some((context, path.to_owned(), path.to_owned()));
    }
    let (root, relative) = roots::containing(&contexts.roots, path)?;
    let ix = contexts.roots.iter().position(|r| std::ptr::eq(r, root))?;
    Some((
        contexts.contexts.get(ix)?.clone(),
        relative.clone(),
        roots::display(root, &relative),
    ))
}

/// Where to read a reference first, in order: (index into the contexts,
/// the path in that root, the resolved path it would be). A session reads
/// it at its checkout; a workspace's own chat at each root it may be in
/// ([`roots::candidates`]). `Err`: it is outside, nothing to read.
fn lookup_order(
    contexts: &LinkContexts,
    path: &str,
) -> Result<Vec<(usize, String, String)>, Resolution> {
    if contexts.multi_root() {
        let found = roots::candidates(&contexts.roots, path);
        if found.is_empty() && path.starts_with('/') {
            let outside = links::resolve(path, "/").map_or_else(|o| o, |r| format!("/{r}"));
            return Err(Resolution::Outside(outside));
        }
        return Ok(found
            .into_iter()
            .map(|(ix, relative)| {
                let open = roots::join(&contexts.roots[ix], &relative);
                (ix, relative, open)
            })
            .collect());
    }
    let Some(context) = contexts.contexts.first() else {
        return Err(Resolution::Missing);
    };
    match links::resolve(path, &context.root) {
        Ok(relative) => Ok(vec![(0, relative.clone(), relative)]),
        Err(outside) => Err(Resolution::Outside(outside)),
    }
}

/// The files of a search that end in `wanted` (whole path components).
fn ending_in(wanted: &str, matches: Vec<FileSearchMatch>) -> Vec<String> {
    let tail = format!("/{wanted}");
    matches
        .into_iter()
        .filter(|hit| !hit.is_dir && (hit.path == wanted || hit.path.ends_with(&tail)))
        .map(|hit| hit.path)
        .collect()
}

/// Resolve `path` in the checkout: read it from the root, else search for
/// files ending in it. A workspace's own chat tries its folder first, then
/// each granted project (an absolute path only in the root it lies in),
/// and searches them all; its resolved paths are absolute
/// ([`roots::join`]). A successful read comes back with the resolution.
/// A `~` path expands with the host's `home` when known, then resolves like
/// an absolute one: only inside the roots ([`links::expand_tilde`]).
async fn resolve(
    engine: &EngineHandle,
    contexts: &LinkContexts,
    path: &str,
    home: Option<&str>,
    cx: &mut AsyncWindowContext,
) -> (
    Resolution,
    Option<(String, Result<FileSnapshot, SharedString>)>,
) {
    let Some(path) = links::expand_tilde(path, home) else {
        return (Resolution::Outside(path.to_owned()), None);
    };
    let path = path.as_str();
    let tries = match lookup_order(contexts, path) {
        Ok(tries) => tries,
        Err(resolution) => return (resolution, None),
    };
    let mut searched = Vec::new();
    for (ix, relative, open) in &tries {
        let Some(context) = contexts.contexts.get(*ix) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        match read_content(engine, context, relative, cx).await {
            Ok(content) => {
                let snapshot = snapshot(relative, content, cx).await;
                return (
                    Resolution::Found(open.clone()),
                    Some((open.clone(), Ok(snapshot))),
                );
            }
            // The device itself is out of reach: a search would fail the same.
            Err(err @ (RpcError::Transport(_) | RpcError::Closed | RpcError::UnknownMethod(_))) => {
                return (Resolution::Failed(crate::files::rpc_message(&err)), None);
            }
            Err(_) => searched.push(open.clone()),
        }
    }
    // An absolute path inside the checkout means exactly that file.
    if path.starts_with('/') || tries.iter().all(|(_, relative, _)| relative.is_empty()) {
        return (Resolution::Missing, None);
    }
    let wanted = path.trim_start_matches("./").trim_start_matches("../");
    let mut found: Vec<(&FilesContext, String, String)> = Vec::new();
    for (ix, context) in contexts.contexts.iter().enumerate() {
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), context.chat_id.clone().into());
        params.insert("query".into(), wanted.into());
        context.address(&mut params);
        let matches = crate::files::call_with_retry(
            engine,
            methods::SEARCH_FILES,
            serde_json::Value::Object(params),
            cx,
        )
        .await
        .and_then(|value| {
            serde_json::from_value::<Vec<FileSearchMatch>>(value)
                .map_err(|e| RpcError::Failed(e.to_string()))
        });
        let matches = match matches {
            Ok(matches) => matches,
            // One project out of reach must not hide another's match.
            Err(_) if contexts.multi_root() => continue,
            Err(err) => return (Resolution::Failed(crate::files::rpc_message(&err)), None),
        };
        for hit in ending_in(wanted, matches) {
            let open = match contexts.roots.get(ix) {
                Some(root) => roots::join(root, &hit),
                None => hit.clone(),
            };
            if !searched.contains(&open) && !found.iter().any(|(_, _, seen)| *seen == open) {
                found.push((context, hit, open));
            }
        }
    }
    match found.len() {
        0 => (Resolution::Missing, None),
        1 => {
            let (context, relative, open) = found.remove(0);
            let read = read_file(engine, context, &relative, cx).await;
            (Resolution::Found(open.clone()), Some((open, read)))
        }
        _ => (
            Resolution::Ambiguous(found.into_iter().map(|(_, _, open)| open).collect()),
            None,
        ),
    }
}

async fn read_content(
    engine: &EngineHandle,
    context: &FilesContext,
    path: &str,
    cx: &mut gpui::AsyncApp,
) -> Result<WorkspaceFileContent, RpcError> {
    let mut params = serde_json::Map::new();
    params.insert("chatId".into(), context.chat_id.clone().into());
    params.insert("cwd".into(), context.cwd.clone().into());
    params.insert("path".into(), path.into());
    context.address(&mut params);
    crate::files::call_with_retry(
        engine,
        methods::READ_WORKSPACE_FILE,
        serde_json::Value::Object(params),
        cx,
    )
    .await
    .and_then(|value| {
        serde_json::from_value::<WorkspaceFileContent>(value)
            .map_err(|e| RpcError::Failed(e.to_string()))
    })
}

/// Split and color a read off the main thread.
async fn snapshot(
    path: &str,
    content: WorkspaceFileContent,
    cx: &mut gpui::AsyncApp,
) -> FileSnapshot {
    let path = path.to_owned();
    cx.background_executor()
        .spawn(async move { FileSnapshot::new(&path, content) })
        .await
}

async fn read_file(
    engine: &EngineHandle,
    context: &FilesContext,
    path: &str,
    cx: &mut gpui::AsyncApp,
) -> Result<FileSnapshot, SharedString> {
    match read_content(engine, context, path, cx).await {
        Ok(content) => Ok(snapshot(path, content, cx).await),
        Err(err) => Err(crate::files::rpc_message(&err)),
    }
}

impl Transcript {
    /// A hovered image reference's picture (or why it can't show), as the
    /// card's body, with its height in code lines.
    fn code_ref_picture(
        &mut self,
        chat_id: &str,
        path: &str,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        use crate::markdown::image::{self, Key, Shown};
        const MAX_H: f32 = 220.0;
        let key = Key {
            scope: chat_id.to_owned().into(),
            src: path.to_owned().into(),
        };
        let src = path.to_owned();
        image::request(
            key.clone(),
            0.0,
            cx,
            |cx| self.load_image(&src, 0.0, cx),
            |_: &mut Self, cx| cx.notify(),
        );
        let lines = |h: f32| h / theme.markdown.code_line_height;
        Some(match image::lookup(&key) {
            Shown::Ready(loaded) => {
                let room = width - 20.0;
                let scale = (room / loaded.width).min(MAX_H / loaded.height).min(1.0);
                let (w, h) = (loaded.width * scale, loaded.height * scale);
                (
                    div()
                        .p(px(10.0))
                        .flex()
                        .justify_center()
                        .child(
                            gpui::img(loaded.image.clone())
                                .w(px(w))
                                .h(px(h))
                                .rounded(px(6.0)),
                        )
                        .into_any_element(),
                    lines(h + 20.0),
                )
            }
            Shown::Pending => (card_note(theme, "Loading…".into(), None), 2.0),
            Shown::Failed(reason) => (card_note(theme, reason, None), 2.0),
        })
    }
}

/// The card's glass tint. Glass themes use the menu tint as is; opaque
/// themes get the same recipe, as the card blurs on every theme. Light's
/// menu tint (0.85) is near-solid there, so it thins enough for the blur to
/// show through while code stays legible.
fn card_tint(theme: &Theme) -> gpui::Hsla {
    match theme.appearance {
        crate::kit::theme::Appearance::Dark => theme.glass_overlay(),
        crate::kit::theme::Appearance::Light => theme.surface_overlay.opacity(0.68),
    }
}

/// A one- or two-line message in place of the excerpt.
fn card_note(theme: &Theme, message: SharedString, detail: Option<SharedString>) -> AnyElement {
    div()
        .px(px(12.0))
        .py(px(10.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .child(message)
        .when_some(detail, |el, detail| {
            el.child(
                div()
                    .mono(theme)
                    .text_size(px(11.5))
                    .text_color(theme.text_faint)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(detail),
            )
        })
        .into_any_element()
}

/// A small ghost button in the card's header.
fn card_button(theme: &Theme, id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(22.0))
        .px(px(7.0))
        .rounded(px(6.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .cursor_pointer()
        .text_size(px(11.5))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.ink(0.08)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_frame_the_cited_lines_with_context() {
        assert_eq!(
            excerpt(400, Some((128, 131))),
            Some(Excerpt {
                first: 125,
                last: 134,
                band: Some((3, 6)),
            })
        );
        // Clamped at both ends of the file.
        assert_eq!(
            excerpt(10, Some((1, 12))),
            Some(Excerpt {
                first: 1,
                last: 10,
                band: Some((0, 9)),
            })
        );
        // A whole file shows its head.
        assert_eq!(
            excerpt(1000, None),
            Some(Excerpt {
                first: 1,
                last: HEAD_LINES,
                band: None,
            })
        );
        // A very long range is cut, its band with it.
        let long = excerpt(5000, Some((100, 1000))).unwrap();
        assert_eq!(long.last - long.first + 1, MAX_EXCERPT_LINES);
        assert_eq!(long.band, Some((3, MAX_EXCERPT_LINES - 1)));
        // Past the end: nothing to show.
        assert_eq!(excerpt(50, Some((51, 51))), None);
    }

    fn contexts(multi: bool) -> LinkContexts {
        let root = |space: Option<&str>, name: &str, path: &str| cypher_proto::ChatFileRoot {
            space_id: space.map(str::to_owned),
            name: name.into(),
            path: path.into(),
            checkout_id: None,
        };
        let context = |root: &cypher_proto::ChatFileRoot| FilesContext {
            chat_id: "c".into(),
            cwd: "/data/bots/b".into(),
            target: None,
            root_space_id: root.space_id.clone(),
            root: root.path.clone(),
        };
        let roots = if multi {
            vec![
                root(None, "Bot", "/data/bots/b"),
                root(Some("api"), "api", "/src/api"),
                root(Some("web"), "web", "/src/web"),
            ]
        } else {
            vec![root(None, "repo", "/work/repo")]
        };
        LinkContexts {
            contexts: roots.iter().map(context).collect(),
            roots: if multi { roots } else { Vec::new() },
        }
    }

    #[test]
    fn a_session_reads_references_at_its_checkout() {
        let session = contexts(false);
        assert_eq!(
            lookup_order(&session, "src/a.rs"),
            Ok(vec![(0, "src/a.rs".into(), "src/a.rs".into())])
        );
        assert_eq!(
            lookup_order(&session, "/work/repo/src/a.rs"),
            Ok(vec![(0, "src/a.rs".into(), "src/a.rs".into())])
        );
        assert_eq!(
            lookup_order(&session, "/etc/hosts"),
            Err(Resolution::Outside("/etc/hosts".into()))
        );
    }

    #[test]
    fn a_workspace_chat_reads_references_in_its_folder_then_each_project() {
        let workspace = contexts(true);
        assert_eq!(
            lookup_order(&workspace, "src/a.rs"),
            Ok(vec![
                (0, "src/a.rs".into(), "/data/bots/b/src/a.rs".into()),
                (1, "src/a.rs".into(), "/src/api/src/a.rs".into()),
                (2, "src/a.rs".into(), "/src/web/src/a.rs".into()),
            ])
        );
        // Absolute: only the root it lies in, and nothing outside them.
        assert_eq!(
            lookup_order(&workspace, "/src/web/app/page.tsx"),
            Ok(vec![(
                2,
                "app/page.tsx".into(),
                "/src/web/app/page.tsx".into()
            )])
        );
        assert_eq!(
            lookup_order(&workspace, "/src/../etc/hosts"),
            Err(Resolution::Outside("/etc/hosts".into()))
        );
        // Each resolved path reads back to its root, under its name.
        let (context, relative, shown) = locate(&workspace, "/src/api/src/a.rs").unwrap();
        assert_eq!(context.root_space_id.as_deref(), Some("api"));
        assert_eq!(
            (relative.as_str(), shown.as_str()),
            ("src/a.rs", "api/src/a.rs")
        );
        assert!(locate(&workspace, "/etc/hosts").is_none());
        // A session's stays relative.
        let (_, relative, shown) = locate(&contexts(false), "src/a.rs").unwrap();
        assert_eq!(
            (relative.as_str(), shown.as_str()),
            ("src/a.rs", "src/a.rs")
        );
    }

    #[test]
    fn a_search_keeps_files_ending_in_the_reference() {
        let hit = |path: &str, is_dir: bool| FileSearchMatch {
            path: path.into(),
            is_dir,
        };
        let hits = vec![
            hit("core/context.py", false),
            hit("lib/core/context.py", false),
            hit("mycore/context.py", false),
            hit("core/context.py.bak", false),
            hit("x/core/context.py", true),
        ];
        assert_eq!(
            ending_in("core/context.py", hits),
            vec![
                "core/context.py".to_string(),
                "lib/core/context.py".to_string()
            ]
        );
    }

    #[test]
    fn file_urls_survive_the_link_parser() {
        for (path, lines) in [
            ("crates/ui/src/a.rs", Some((12, 20))),
            ("docs/100%/a#b?.md", Some((3, 3))),
            ("Makefile", None),
        ] {
            let link = links::parse(&file_url(path, lines)).unwrap();
            assert_eq!(link.path, path);
            assert_eq!(link.line, lines.map(|(first, _)| first));
            assert_eq!(
                link.end_line,
                lines.and_then(|(first, last)| (last > first).then_some(last))
            );
        }
    }

    #[test]
    fn snapshots_split_lines_like_the_editor() {
        let content = |text: &str| WorkspaceFileContent {
            text: Some(text.into()),
            bytes: text.len() as u64,
            binary: false,
            truncated: false,
        };
        assert_eq!(
            FileSnapshot::new("a.txt", content("a\nb\nc")).line_count(),
            3
        );
        // A final newline ends the last line; it does not start another.
        assert_eq!(
            FileSnapshot::new("a.txt", content("a\nb\n")).line_count(),
            2
        );
        assert_eq!(FileSnapshot::new("a.txt", content("")).line_count(), 1);
    }
}
