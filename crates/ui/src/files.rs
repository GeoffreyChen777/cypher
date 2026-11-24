//! The right pane's Files surface: a file manager over the selected chat's
//! checkout. A lazy directory tree (the engine's jailed `ListWorkspaceFiles`
//! browser — built for iOS, now shared) with multi-select, a context menu,
//! keyboard control, inline rename/new entries, cut/copy/paste, drag moves,
//! Finder imports, a go-to-file filter and git status marks; beside it the
//! picked file: a syntax-colored [`editor::CodeEditor`] saving back through
//! `WriteWorkspaceFile`, rendered Markdown/SVG/HTML previews, an image
//! viewer, a PDF viewer, or an info card and hex dump for other binaries.
//!
//! Everything is fetched from the chat's HOST device (`targetDeviceId` relay
//! forwarding, exactly like the diff pane): the tree is that checkout's, not
//! this machine's. Directories load on first expand; a file loads on first
//! open and keeps its editor (and any unsaved text) for the surface's life.
//! The checkout's diff stream drives git marks and quiet refreshes when the
//! agent changes files.
//!
//! Layout adapts to the pane: wide panes show tree + editor side by side;
//! narrow ones show one at a time, toggled from the header.
//!
//! Markdown files open in a rendered PREVIEW (the transcript's own Markdown
//! renderer, fenced code highlighted in the background); a header toggle
//! flips to the source editor and back. The preview re-parses lazily — only
//! when it is shown and the text changed since the last parse. HTML files
//! open the same way, as a live page in a native web view (macOS). PDFs open
//! in that view too: read from disk when the checkout is on this machine,
//! fetched as bytes from another device's.

pub mod editor;
pub mod links;
mod menu;
mod ops;
mod preview;
pub mod root_picker;
pub mod roots;
mod tree;
pub mod web_view;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable as _,
    Image, IntoElement, MouseButton, MouseDownEvent, Pixels, Point, Render, ScrollStrategy,
    SharedString, Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px,
    uniform_list,
};

use cypher_proto::{
    CheckoutDiff, FileSearchMatch, WorkspaceDirectory, WorkspaceFileContent, WorkspaceFileEntry,
};
use cypher_rpc::{RpcError, methods};
use cypher_syntax::{HighlightedDocument, LanguageId};

use crate::kit::icons::{self, icon};
use crate::kit::live_layer::LiveLayer;
use crate::kit::popover::{Loadable, Popup};
use crate::kit::theme::Theme;
use crate::markdown::parser::{Block, BlockTree, parse_full};
use crate::markdown::render;
use crate::markdown::selection::{self, SelectionScope};
use crate::state::{AppState, EngineHandle};
use crate::widgets::text_input::{TextInput, TextInputEvent};

use editor::{CodeEditor, CodeEditorEvent};
use preview::{FileKind, Media, Zoom};
use tree::{FlattenOptions, GitMarks, InlineEdit, Selection, TreeRow, parent_of};

/// Tree column width when the pane is wide enough to split (until dragged).
pub const TREE_WIDTH: f32 = 240.0;
/// The seam drag keeps both sides usable.
const TREE_MIN_WIDTH: f32 = 160.0;
const EDITOR_MIN_WIDTH: f32 = 280.0;
/// The tree column's last dragged width — new Files tabs start there.
static LAST_TREE_WIDTH: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(TREE_WIDTH.to_bits());
/// Below this pane width the tree and the editor take turns.
pub const SPLIT_MIN_WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 24.0;
const INDENT: f32 = 14.0;
/// Breathing room between the tree column's edges and a row's hover/selected
/// pill. The list carries it as padding, so rows still fill the width inside.
const ROW_INSET: f32 = 6.0;
const ROW_RADIUS: f32 = 6.0;
const TOOLBAR_HEIGHT: f32 = 34.0;
/// Images kept decoded at once; older ones drop and reload when reopened.
const MAX_IMAGES: usize = 8;

/// Drag marker for the tree/file seam; carries its panel so other Files tabs
/// ignore the drag.
struct TreeResize(gpui::EntityId);

/// Resize drags draw nothing at the pointer.
struct ResizeGhost;

impl Render for ResizeGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// The tree column's width for a `pane` this wide: the wanted width, kept
/// within [`TREE_MIN_WIDTH`] and leaving [`EDITOR_MIN_WIDTH`] for the file.
fn clamp_tree_width(wanted: f32, pane: f32) -> f32 {
    wanted.min(pane - EDITOR_MIN_WIDTH).max(TREE_MIN_WIDTH)
}

/// What the Files surface asks of its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesEvent {
    /// "Mention in Chat": put a file chip in the session's composer.
    Mention { path: String, is_dir: bool },
}

/// The request context a Files surface is bound to — the selected chat's
/// checkout on its host device (a workspace's own chat: the root its
/// picker shows). Re-read per request (like the diff pane) so the engine's
/// own "chat device or checkout changed" guard is the only thing that can
/// go stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesContext {
    pub chat_id: String,
    /// The chat's own folder: every request carries it (the stale check).
    pub cwd: String,
    /// The host device when it is not the connected engine's own.
    pub target: Option<String>,
    /// A workspace's own chat showing a granted project: that project
    /// (sent as `rootSpaceId`). `None`: the chat's own folder.
    pub root_space_id: Option<String>,
    /// The folder paths are relative to: `cwd` for a session; the shown
    /// root's canonical folder for a workspace's own chat.
    pub root: String,
}

impl FilesContext {
    fn params(&self, path: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), self.chat_id.clone().into());
        params.insert("cwd".into(), self.cwd.clone().into());
        params.insert("path".into(), path.to_string().into());
        self.address(&mut params);
        params
    }

    /// Aim a request at this context's host and root (every file RPC and
    /// `SearchFiles` with `chatId`).
    pub(crate) fn address(&self, params: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(target) = &self.target {
            params.insert("targetDeviceId".into(), target.clone().into());
        }
        if let Some(space_id) = &self.root_space_id {
            params.insert("rootSpaceId".into(), space_id.clone().into());
        }
    }
}

/// Resolve the Files context from app state. Pure over the state's rows
/// (and, for a workspace's own chat, its listed roots).
pub fn context_for(state: &AppState) -> Result<FilesContext, &'static str> {
    let chat = state
        .selected_chat_row()
        .ok_or("Open a session to browse its files.")?;
    let cwd = chat
        .cwd
        .clone()
        .ok_or("This session has no project folder.")?;
    let target = (state.local_device_id.as_deref() != Some(chat.device_id.as_str()))
        .then(|| chat.device_id.clone());
    if state.workspace_chat_selected() {
        if state.file_roots() == crate::state::FileRoots::Loading {
            return Err("Loading…");
        }
        let root = state
            .files_root()
            .ok_or("This workspace has no folder to browse.")?;
        return Ok(context_at(chat, cwd, target, root));
    }
    if chat.space_id.is_none() {
        return Err("This session is not attached to a project.");
    }
    Ok(FilesContext {
        chat_id: chat.id.clone(),
        root: cwd.clone(),
        cwd,
        target,
        root_space_id: None,
    })
}

fn context_at(
    chat: &cypher_proto::Chat,
    cwd: String,
    target: Option<String>,
    root: &cypher_proto::ChatFileRoot,
) -> FilesContext {
    FilesContext {
        chat_id: chat.id.clone(),
        cwd,
        target,
        root_space_id: root.space_id.clone(),
        root: root.path.clone(),
    }
}

/// Where a chat's file references may be looked up, in the order to try:
/// a session's checkout alone; a workspace's own chat's folder, then each
/// granted project ([`roots::candidates`]). `Err` says why there is none.
pub fn link_contexts(state: &AppState) -> Result<LinkContexts, &'static str> {
    if !state.workspace_chat_selected() {
        return context_for(state).map(|context| LinkContexts {
            roots: Vec::new(),
            contexts: vec![context],
        });
    }
    let chat = state
        .selected_chat_row()
        .ok_or("Open a session to browse its files.")?;
    let cwd = chat
        .cwd
        .clone()
        .ok_or("This workspace has no folder to browse.")?;
    let target = (state.local_device_id.as_deref() != Some(chat.device_id.as_str()))
        .then(|| chat.device_id.clone());
    let crate::state::FileRoots::Ready(list) = state.file_roots() else {
        return Err("Loading…");
    };
    Ok(LinkContexts {
        roots: list.to_vec(),
        contexts: list
            .iter()
            .map(|root| context_at(chat, cwd.clone(), target.clone(), root))
            .collect(),
    })
}

/// [`link_contexts`]: one context per root, in lookup order.
#[derive(Debug, Clone)]
pub struct LinkContexts {
    /// A workspace's own chat: its roots, matching `contexts`. Empty for a
    /// session, whose paths stay relative to its checkout.
    pub roots: Vec<cypher_proto::ChatFileRoot>,
    pub contexts: Vec<FilesContext>,
}

impl LinkContexts {
    /// Several roots: references travel as absolute paths.
    pub fn multi_root(&self) -> bool {
        !self.roots.is_empty()
    }
}

/// Where a file's source opens once its text is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoTo {
    /// A chat link's lines (first, last), selected.
    Lines(usize, usize),
    /// A restored tab's caret line.
    Caret(usize),
}

impl GoTo {
    fn apply(self, editor: &mut CodeEditor, cx: &mut Context<CodeEditor>) {
        match self {
            Self::Lines(first, last) => editor.go_to_lines(first, last, cx),
            Self::Caret(line) => editor.go_to_caret_line(line, cx),
        }
    }

    fn line(self) -> usize {
        match self {
            Self::Lines(first, _) | Self::Caret(first) => first,
        }
    }
}

/// A saved Files tab on its way back ([`FilesPanel::restore`]).
#[derive(Debug, Clone)]
struct Restore {
    root: Option<String>,
    file: String,
    line: Option<usize>,
}

#[derive(Debug, Default)]
struct DirState {
    entries: Vec<WorkspaceFileEntry>,
    truncated: bool,
    loading: bool,
    loaded: bool,
    error: Option<SharedString>,
}

struct OpenFile {
    editor: Entity<CodeEditor>,
    kind: FileKind,
    /// The text as last loaded or saved — the dirty baseline.
    clean: Option<String>,
    dirty: bool,
    loading: bool,
    saving: bool,
    binary: bool,
    truncated: bool,
    bytes: u64,
    error: Option<SharedString>,
    /// Changed on disk while not shown: reload when it is next opened.
    stale: bool,
    /// Markdown/SVG/HTML: show the rendered preview instead of the source.
    preview: bool,
    /// The last parsed preview; `preview_stale` marks it behind the text.
    preview_tree: Option<Arc<BlockTree>>,
    preview_stale: bool,
    /// Fenced-code highlights by top-level block index, for `preview_tree`.
    preview_highlights: HashMap<usize, Arc<HighlightedDocument>>,
    preview_gen: u64,
    _preview_task: Option<Task<()>>,
    /// SVG: the rendered source as of the last preview, and its declared
    /// size.
    svg: Option<Arc<Image>>,
    svg_dims: Option<(u32, u32)>,
    /// Images, other devices' PDFs, and binaries' hex dumps.
    media: Media,
    zoom: Zoom,
    /// Binaries: the hex dump instead of the info card.
    hex: bool,
    hex_scroll: UniformListScrollHandle,
    _media_task: Option<Task<()>>,
    _sub: Subscription,
}

/// Whether `path` opens in the Markdown preview (by extension/filename,
/// the syntax crate's registry).
fn is_markdown(path: &str) -> bool {
    cypher_syntax::language_for_path(path) == Some(LanguageId::Markdown)
}

/// The go-to-file field and its results (the engine's ranked file search).
struct Filter {
    input: Entity<TextInput>,
    query: String,
    results: Loadable<Vec<FileSearchMatch>>,
    active: usize,
    scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
    _sub: Subscription,
}

/// The footer's one line: progress, a result, or an error.
struct Status {
    text: SharedString,
    error: bool,
    seq: u64,
}

pub struct FilesPanel {
    state: Entity<AppState>,
    dirs: HashMap<String, DirState>,
    expanded: HashSet<String>,
    rows: Vec<TreeRow>,
    tree_scroll: UniformListScrollHandle,
    /// The file the editor side shows.
    selected: Option<String>,
    open_files: HashMap<String, OpenFile>,
    /// Narrow panes: which half shows. Wide panes: whether the tree column
    /// is collapsed.
    tree_visible: bool,
    pane_width: Rc<Cell<f32>>,
    /// The dock's rail floats over the pane's top-right corner; a column
    /// touching the right edge keeps clear of it.
    header_right: f32,
    /// The tree column's wanted width in wide panes (the seam drag).
    tree_width: f32,
    /// Bumped whenever the checkout context changes so stale replies drop.
    generation: u64,
    context: Option<FilesContext>,
    context_error: Option<SharedString>,
    /// Keeps directory fetches alive; keyed so a re-request replaces the
    /// previous one for the same folder.
    dir_tasks: HashMap<String, Task<()>>,
    file_tasks: HashMap<String, Task<()>>,
    /// The Markdown preview's own selection scope (registry reset per frame).
    selection_scope: SelectionScope,
    /// The HTML preview's page, made on first use.
    web: Option<web_view::WebView>,
    // ---- file manager ----
    tree_focus: FocusHandle,
    confirm_focus: FocusHandle,
    confirm_focus_pending: bool,
    selection: Selection,
    show_hidden: bool,
    edit: Option<InlineEdit>,
    clipboard: Option<ops::FileClipboard>,
    menu: Popup<menu::MenuState>,
    /// A workspace's own chat: the project picker in the header.
    root_menu: Popup<()>,
    confirm: Option<menu::DeleteConfirm>,
    status: Option<Status>,
    status_seq: u64,
    /// The status whose fade timer is already running.
    status_fade: u64,
    /// In-flight mutations (kept alive until they finish).
    op_tasks: Vec<Task<()>>,
    /// Scroll this row into view once it is in the tree.
    reveal_pending: Option<String>,
    /// Focus the shown file's editor once it has loaded.
    focus_editor_pending: bool,
    /// Go to this line of this file once it has loaded (a chat link, or
    /// a restored tab's caret).
    line_pending: Option<(String, GoTo)>,
    /// A saved tab coming back ([`Self::restore`]): its file opens once the
    /// context it was saved in is back and the file is still there.
    restore_pending: Option<Restore>,
    restore_task: Option<Task<()>>,
    filter: Filter,
    /// Decoded images, oldest first (bounded by [`MAX_IMAGES`]).
    image_order: Vec<String>,
    evicted: Vec<Arc<Image>>,
    // ---- checkout diff stream: git marks + quiet refreshes ----
    git: GitMarks,
    diffs: Vec<CheckoutDiff>,
    diff_checksum: Option<String>,
    diff_paths: HashSet<String>,
    watch_target: Option<Option<String>>,
    watch_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    /// Loaders and hover fades inside the cached panel (see
    /// [`Self::live_layer`]).
    live: LiveLayer,
    _observe: Subscription,
}

impl EventEmitter<FilesEvent> for FilesPanel {}

impl FilesPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
        let input = cx.new(|cx| {
            let mut input = TextInput::with_context("Go to file…", "PaletteSearch", cx);
            input.set_metrics(12.0, 16.0);
            input
        });
        let filter_sub = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, TextInputEvent::Edited) {
                this.on_filter_edited(cx);
            }
        });
        Self {
            state,
            dirs: HashMap::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            tree_scroll: UniformListScrollHandle::new(),
            selected: None,
            open_files: HashMap::new(),
            tree_visible: true,
            pane_width: Rc::new(Cell::new(0.0)),
            header_right: 8.0,
            tree_width: f32::from_bits(LAST_TREE_WIDTH.load(std::sync::atomic::Ordering::Relaxed)),
            generation: 0,
            context: None,
            context_error: None,
            dir_tasks: HashMap::new(),
            file_tasks: HashMap::new(),
            selection_scope: selection::next_preview_scope(),
            web: None,
            tree_focus: cx.focus_handle(),
            confirm_focus: cx.focus_handle(),
            confirm_focus_pending: false,
            selection: Selection::default(),
            show_hidden: true,
            edit: None,
            clipboard: None,
            menu: Popup::default(),
            root_menu: Popup::default(),
            confirm: None,
            status: None,
            status_seq: 0,
            status_fade: 0,
            op_tasks: Vec::new(),
            reveal_pending: None,
            focus_editor_pending: false,
            line_pending: None,
            restore_pending: None,
            restore_task: None,
            filter: Filter {
                input,
                query: String::new(),
                results: Loadable::Idle,
                active: 0,
                scroll: UniformListScrollHandle::new(),
                task: None,
                _sub: filter_sub,
            },
            image_order: Vec::new(),
            evicted: Vec::new(),
            git: GitMarks::default(),
            diffs: Vec::new(),
            diff_checksum: None,
            diff_paths: HashSet::new(),
            watch_target: None,
            watch_task: None,
            refresh_task: None,
            live: LiveLayer::default(),
            _observe: observe,
        }
    }

    /// The tab strip's label: the open file's name, else "Files".
    pub fn tab_title(&self) -> SharedString {
        match self.selected.as_deref().and_then(|p| p.rsplit('/').next()) {
            Some(name) if !name.is_empty() => {
                let dirty = self
                    .selected
                    .as_deref()
                    .and_then(|p| self.open_files.get(p))
                    .is_some_and(|f| f.dirty);
                if dirty {
                    format!("● {name}").into()
                } else {
                    name.to_string().into()
                }
            }
            _ => "Files".into(),
        }
    }

    /// How far the dock's rail reaches in from the right edge.
    pub fn set_header_right(&mut self, inset: f32, cx: &mut Context<Self>) {
        if self.header_right != inset {
            self.header_right = inset;
            cx.notify();
        }
    }

    /// The folder a toolbar "new" lands in: the selected folder, else the
    /// selected file's folder, else the root.
    fn folder_for_new(&self) -> String {
        match self.selection.resolve(&self.rows).as_slice() {
            [(path, true)] => path.clone(),
            [(path, false)] => parent_of(path).to_string(),
            _ => String::new(),
        }
    }

    /// Idempotent: bind to the selected chat and load the root listing.
    pub fn ensure_content(&mut self, cx: &mut Context<Self>) {
        self.sync(cx);
        if self.context.is_some() && !self.dirs.contains_key("") {
            self.load_dir(String::new(), cx);
        }
    }

    /// Re-resolve the context; a changed checkout drops everything loaded
    /// (the engine would refuse the old paths anyway).
    fn sync(&mut self, cx: &mut Context<Self>) {
        let next = context_for(self.state.read(cx));
        match next {
            Ok(context) => {
                if self.context.as_ref() != Some(&context) {
                    if self.context.is_some() {
                        self.reset();
                    }
                    self.context = Some(context);
                    self.context_error = None;
                    if !self.dirs.contains_key("") {
                        self.load_dir(String::new(), cx);
                    }
                    cx.notify();
                }
                self.ensure_watch(cx);
                self.apply_diffs(cx);
                self.apply_restore(cx);
            }
            Err(message) => {
                if self.context.is_some() || self.context_error.is_none() {
                    self.context = None;
                    self.context_error = Some(message.into());
                    cx.notify();
                }
            }
        }
    }

    fn reset(&mut self) {
        self.generation += 1;
        self.dirs.clear();
        self.expanded.clear();
        self.rows.clear();
        self.selected = None;
        self.line_pending = None;
        for (_, file) in self.open_files.drain() {
            if let Media::Image { image, .. } = file.media {
                self.evicted.push(image);
            }
        }
        self.dir_tasks.clear();
        self.file_tasks.clear();
        self.selection.clear();
        self.edit = None;
        self.clipboard = None;
        self.confirm = None;
        self.status = None;
        self.op_tasks.clear();
        self.image_order.clear();
        self.git = GitMarks::default();
        self.diff_checksum = None;
        self.diff_paths.clear();
    }

    fn engine_and_context(&self, cx: &App) -> Option<(EngineHandle, FilesContext)> {
        Some((
            self.state.read(cx).engine().cloned()?,
            self.context.clone()?,
        ))
    }

    fn keep_task(&mut self, task: Task<()>) {
        self.op_tasks.retain(|task| !task.is_ready());
        self.op_tasks.push(task);
    }

    /// Footer text; a plain result fades after a few seconds.
    fn set_status(&mut self, text: Option<String>) {
        self.status_seq += 1;
        self.status = text.map(|text| Status {
            text: text.into(),
            error: false,
            seq: self.status_seq,
        });
    }

    fn set_error(&mut self, text: SharedString) {
        self.status_seq += 1;
        self.status = Some(Status {
            text,
            error: true,
            seq: self.status_seq,
        });
    }

    fn rebuild_rows(&mut self) {
        let creating = match self.edit.as_ref().map(|edit| &edit.target) {
            Some(tree::EditTarget::Create { parent, directory }) => {
                Some((parent.as_str(), *directory))
            }
            _ => None,
        };
        self.rows = tree::flatten_tree(
            &self.dirs,
            &self.expanded,
            FlattenOptions {
                show_hidden: self.show_hidden,
                creating,
            },
        );
        if let Some(path) = self.reveal_pending.clone()
            && let Some(ix) = self.rows.iter().position(|row| row.path == path)
        {
            self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            self.reveal_pending = None;
        }
    }

    fn scroll_to_edit(&self) {
        let ix = self.rows.iter().position(|row| {
            row.create.is_some()
                || matches!(
                    self.edit.as_ref().map(|e| &e.target),
                    Some(tree::EditTarget::Rename { path, .. }) if *path == row.path
                )
        });
        if let Some(ix) = ix {
            self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
    }

    fn load_dir(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        {
            let dir = self.dirs.entry(path.clone()).or_default();
            dir.loading = true;
            dir.error = None;
        }
        self.rebuild_rows();
        let params = context.params(&path);
        let task_path = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = call_with_retry(
                &engine,
                methods::LIST_WORKSPACE_FILES,
                serde_json::Value::Object(params),
                cx,
            )
            .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let dir = this.dirs.entry(task_path.clone()).or_default();
                dir.loading = false;
                match result.and_then(|value| {
                    serde_json::from_value::<WorkspaceDirectory>(value)
                        .map_err(|e| RpcError::Failed(e.to_string()))
                }) {
                    Ok(listing) => {
                        dir.entries = listing.entries;
                        dir.truncated = listing.truncated;
                        dir.loaded = true;
                        dir.error = None;
                    }
                    Err(err) => {
                        dir.error = Some(rpc_message(&err));
                    }
                }
                this.dir_tasks.remove(&task_path);
                this.rebuild_rows();
                cx.notify();
            })
            .ok();
        });
        self.dir_tasks.insert(path, task);
        cx.notify();
    }

    /// Re-list folders whose contents changed (when they are listed at all);
    /// expanded-but-unloaded folders on the way load too.
    fn reload_dirs(&mut self, dirs: impl IntoIterator<Item = String>, cx: &mut Context<Self>) {
        let mut seen = HashSet::new();
        for dir in dirs {
            if seen.insert(dir.clone())
                && (dir.is_empty() || self.dirs.contains_key(&dir) || self.expanded.contains(&dir))
            {
                self.load_dir(dir, cx);
            }
        }
        let missing: Vec<String> = self
            .expanded
            .iter()
            .filter(|dir| !self.dirs.contains_key(*dir))
            .cloned()
            .collect();
        for dir in missing {
            self.load_dir(dir, cx);
        }
        self.rebuild_rows();
    }

    fn toggle_dir(&mut self, path: String, cx: &mut Context<Self>) {
        if self.expanded.contains(&path) {
            self.expanded.remove(&path);
        } else {
            self.expanded.insert(path.clone());
            if !self.dirs.get(&path).is_some_and(|d| d.loaded || d.loading) {
                self.load_dir(path, cx);
            }
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn set_expanded(&mut self, path: &str, open: bool, cx: &mut Context<Self>) {
        if self.expanded.contains(path) != open {
            self.toggle_dir(path.to_string(), cx);
        }
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.rebuild_rows();
        cx.notify();
    }

    /// Expand `path`'s folders (loading them as needed), select it, and
    /// scroll it into view once its row exists.
    fn reveal_in_tree(&mut self, path: String, cx: &mut Context<Self>) {
        self.expand_ancestors(&path);
        let missing: Vec<String> = self
            .expanded
            .iter()
            .filter(|dir| !self.dirs.get(*dir).is_some_and(|d| d.loaded || d.loading))
            .cloned()
            .collect();
        for dir in missing {
            self.load_dir(dir, cx);
        }
        self.selection.single(&path);
        self.reveal_pending = Some(path);
        self.rebuild_rows();
        cx.notify();
    }

    /// A chat link: reveal `path` (relative to the checkout, `""` the root)
    /// in the tree and open it — a folder expands, a file shows `lines`
    /// (first and last, selected) when given (in its source, not a preview).
    pub fn open_link(
        &mut self,
        path: String,
        lines: Option<(usize, usize)>,
        dir: bool,
        cx: &mut Context<Self>,
    ) {
        self.ensure_content(cx);
        if path.is_empty() {
            return;
        }
        self.clear_filter(cx);
        self.reveal_in_tree(path.clone(), cx);
        if dir || self.entry_meta(&path).is_some_and(|entry| entry.is_dir) {
            self.set_expanded(&path, true, cx);
            return;
        }
        self.open_file(path.clone(), false, None, cx);
        if let Some((first, last)) = lines {
            self.go_to(path, GoTo::Lines(first, last), cx);
        }
    }

    /// Show `path`'s source at `go` (now, or once its text is in).
    fn go_to(&mut self, path: String, go: GoTo, cx: &mut Context<Self>) {
        let Some(file) = self.open_files.get_mut(&path) else {
            return;
        };
        file.preview = false;
        if file.clean.is_some() && !file.loading {
            file.editor.update(cx, |editor, cx| go.apply(editor, cx));
        } else {
            self.line_pending = Some((path, go));
        }
    }

    /// Bring a saved tab back ([`crate::prefs::SavedSurface::Files`]): `file`
    /// (relative to root `root`) opens, its source at `line` when given,
    /// once this tab's context is in — and only if that context is still on
    /// the same root and the file still exists; otherwise the tab stays on
    /// its tree, quietly.
    pub fn restore(
        &mut self,
        root: Option<String>,
        file: Option<String>,
        line: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.restore_pending =
            file.filter(|file| !file.is_empty())
                .map(|file| Restore { root, file, line });
        self.sync(cx);
    }

    fn apply_restore(&mut self, cx: &mut Context<Self>) {
        if self.restore_task.is_some() {
            return;
        }
        let (Some(context), Some(restore)) = (self.context.clone(), self.restore_pending.as_ref())
        else {
            return;
        };
        if restore.root != context.root_space_id || self.selected.is_some() {
            // Another root shows now (its grant went, or the pick fell
            // back), or the owner opened a file meanwhile.
            self.restore_pending = None;
            return;
        }
        let file = restore.file.clone();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            // Nothing to ask (no engine yet): open it as a link would.
            return self.open_restored(cx);
        };
        self.restore_task = Some(cx.spawn(async move |this, cx| {
            let exists = entry_exists(&engine, &context, &file, cx).await;
            this.update(cx, |this, cx| {
                this.restore_task = None;
                if this.context.as_ref() != Some(&context) {
                    // The context moved on while we asked: judge again.
                    return this.apply_restore(cx);
                }
                if exists && this.selected.is_none() {
                    this.open_restored(cx);
                } else {
                    this.restore_pending = None;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn open_restored(&mut self, cx: &mut Context<Self>) {
        let Some(Restore { file, line, .. }) = self.restore_pending.take() else {
            return;
        };
        self.reveal_in_tree(file.clone(), cx);
        self.open_file(file.clone(), false, None, cx);
        if let Some(line) = line {
            self.go_to(file, GoTo::Caret(line), cx);
        }
    }

    /// What identifies this tab's view, for the session's saved docks: the
    /// file it shows, in the root it shows, and the caret's line while its
    /// source shows (a tab still coming back reports what it will show).
    pub fn saved_view(&self, cx: &App) -> crate::prefs::SavedSurface {
        if let Some(restore) = &self.restore_pending {
            return crate::prefs::SavedSurface::Files {
                root: restore.root.clone(),
                file: Some(restore.file.clone()),
                line: restore.line,
            };
        }
        let file = self.selected.clone();
        let line = file.as_ref().and_then(|path| {
            if let Some((pending, go)) = &self.line_pending
                && pending == path
            {
                return Some(go.line());
            }
            let open = self.open_files.get(path)?;
            (!open.preview && open.clean.is_some()).then(|| open.editor.read(cx).caret_position().0)
        });
        crate::prefs::SavedSurface::Files {
            root: self
                .context
                .as_ref()
                .and_then(|context| context.root_space_id.clone()),
            file,
            line: line.filter(|line| *line > 1),
        }
    }

    /// Show `path` in the editor side, loading it on first open. Narrow
    /// panes flip to the editor; `focus` moves typing into it.
    fn open_file(
        &mut self,
        path: String,
        focus: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(path.clone());
        if self.pane_width.get() < SPLIT_MIN_WIDTH {
            self.tree_visible = false;
        }
        if !self.open_files.contains_key(&path) {
            let editor = cx.new(|cx| CodeEditor::new(&path, cx));
            let sub = cx.subscribe(&editor, |this: &mut Self, editor, event, cx| {
                let Some(path) = this
                    .open_files
                    .iter()
                    .find(|(_, file)| file.editor == editor)
                    .map(|(path, _)| path.clone())
                else {
                    return;
                };
                match event {
                    CodeEditorEvent::Edited => {
                        if let Some(file) = this.open_files.get_mut(&path) {
                            file.preview_stale = true;
                            let dirty = file
                                .clean
                                .as_deref()
                                .is_none_or(|clean| clean != editor.read(cx).content());
                            if file.dirty != dirty {
                                file.dirty = dirty;
                                cx.notify();
                            }
                        }
                    }
                    CodeEditorEvent::Save => this.save_file(path, cx),
                }
            });
            let kind = FileKind::for_path(&path);
            self.open_files.insert(
                path.clone(),
                OpenFile {
                    editor,
                    kind,
                    clean: None,
                    dirty: false,
                    loading: false,
                    saving: false,
                    binary: false,
                    truncated: false,
                    bytes: 0,
                    error: None,
                    stale: false,
                    preview: kind.previews(),
                    preview_tree: None,
                    preview_stale: true,
                    preview_highlights: HashMap::new(),
                    preview_gen: 0,
                    _preview_task: None,
                    svg: None,
                    svg_dims: None,
                    media: Media::Idle,
                    zoom: Zoom::Fit,
                    hex: false,
                    hex_scroll: UniformListScrollHandle::new(),
                    _media_task: None,
                    _sub: sub,
                },
            );
            match kind {
                FileKind::Image(_) => {}
                FileKind::Pdf => self.load_pdf(path.clone(), cx),
                _ => self.load_file(path.clone(), cx),
            }
        } else if let Some(file) = self.open_files.get_mut(&path)
            && std::mem::take(&mut file.stale)
            && !file.dirty
        {
            let kind = file.kind;
            match kind {
                FileKind::Image(_) => file.media = Media::Idle,
                FileKind::Pdf => self.load_pdf(path.clone(), cx),
                _ => self.load_file(path.clone(), cx),
            }
        }
        if let Some(file) = self.open_files.get(&path)
            && matches!(file.kind, FileKind::Image(_))
            && matches!(file.media, Media::Idle)
        {
            self.load_image(path.clone(), cx);
        }
        if focus {
            match window {
                Some(window)
                    if self
                        .open_files
                        .get(&path)
                        .is_some_and(|f| f.clean.is_some()) =>
                {
                    let file = &self.open_files[&path];
                    if !file.binary && !(file.preview && file.kind.previews()) {
                        let handle = file.editor.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                    }
                }
                // Not loaded yet: focus once it is (if it is text).
                _ => {
                    self.focus_editor_pending =
                        !self.open_files.get(&path).is_some_and(|f| f.kind.media())
                }
            }
        }
        cx.notify();
    }

    /// (Re)read a file's text. A reload that finds the text unchanged keeps
    /// the editor (caret, scroll, undo) untouched.
    fn load_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        if let Some(file) = self.open_files.get_mut(&path) {
            file.loading = true;
            file.error = None;
        }
        let params = context.params(&path);
        let task_path = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = call_with_retry(
                &engine,
                methods::READ_WORKSPACE_FILE,
                serde_json::Value::Object(params),
                cx,
            )
            .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.file_tasks.remove(&task_path);
                let Some(file) = this.open_files.get_mut(&task_path) else {
                    return;
                };
                file.loading = false;
                match result.and_then(|value| {
                    serde_json::from_value::<WorkspaceFileContent>(value)
                        .map_err(|e| RpcError::Failed(e.to_string()))
                }) {
                    Ok(content) => {
                        file.binary = content.binary;
                        file.truncated = content.truncated;
                        file.bytes = content.bytes;
                        file.error = None;
                        let text = content.text.unwrap_or_default();
                        // Unchanged text keeps the editor as it is.
                        if file.clean.as_deref() != Some(text.as_str()) || file.dirty {
                            // A truncated read must never be saved back — it
                            // would silently drop the file's tail.
                            let read_only = content.binary || content.truncated;
                            file.clean = Some(text.clone());
                            file.dirty = false;
                            file.preview_stale = true;
                            file.editor
                                .update(cx, |editor, cx| editor.set_content(text, read_only, cx));
                        }
                    }
                    Err(err) => {
                        file.error = Some(rpc_message(&err));
                    }
                }
                // A chat link's line lands once the text is in.
                if let Some((_, go)) = this.line_pending.take_if(|(path, _)| *path == task_path)
                    && let Some(file) = this.open_files.get(&task_path)
                    && file.clean.is_some()
                {
                    file.editor.update(cx, |editor, cx| go.apply(editor, cx));
                }
                cx.notify();
            })
            .ok();
        });
        self.file_tasks.insert(path, task);
        cx.notify();
    }

    fn save_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let Some(file) = self.open_files.get_mut(&path) else {
            return;
        };
        if file.saving || file.loading || file.editor.read(cx).read_only() {
            return;
        }
        let text = file.editor.read(cx).content().to_string();
        file.saving = true;
        file.error = None;
        let mut params = context.params(&path);
        params.insert("text".into(), text.clone().into());
        let task_path = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::WRITE_WORKSPACE_FILE,
                    serde_json::Value::Object(params),
                )
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.file_tasks.remove(&task_path);
                let Some(file) = this.open_files.get_mut(&task_path) else {
                    return;
                };
                file.saving = false;
                match result {
                    Ok(_) => {
                        file.clean = Some(text.clone());
                        file.bytes = text.len() as u64;
                        // Edits made while the save was in flight stay dirty.
                        file.dirty = file.editor.read(cx).content() != text;
                        file.error = None;
                    }
                    Err(err) => {
                        file.error = Some(rpc_message(&err));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.file_tasks.insert(path, task);
        cx.notify();
    }

    /// Fetch an image's bytes and decode its size in the background.
    fn load_image(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let Some(file) = self.open_files.get_mut(&path) else {
            return;
        };
        let FileKind::Image(format) = file.kind else {
            return;
        };
        file.media = Media::Loading {
            received: 0,
            total: 0,
        };
        let params = context.params(&path);
        let task_path = path.clone();
        file._media_task = Some(cx.spawn(async move |this, cx| {
            let progress_this = this.clone();
            let mut progress_cx = cx.clone();
            let progress_path = task_path.clone();
            let read = preview::read_bytes(
                &engine,
                params,
                preview::IMAGE_LIMIT,
                false,
                move |received, total| {
                    progress_this
                        .update(&mut progress_cx, |this, cx| {
                            if let Some(file) = this.open_files.get_mut(&progress_path) {
                                file.media = Media::Loading { received, total };
                                cx.notify();
                            }
                        })
                        .ok();
                },
            )
            .await;
            let decoded = match read {
                Ok((bytes, total)) => {
                    let dims = cx
                        .background_executor()
                        .spawn(async move {
                            let dims = preview::image_dims(&bytes);
                            (bytes, dims)
                        })
                        .await;
                    Ok((dims, total))
                }
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let Some(file) = this.open_files.get_mut(&task_path) else {
                    return;
                };
                match decoded {
                    Ok(((bytes, dims), total)) => {
                        file.bytes = total;
                        file.media = Media::Image {
                            image: Arc::new(Image::from_bytes(format, bytes)),
                            dims,
                        };
                        this.note_image(task_path.clone());
                    }
                    Err(error) => file.media = Media::Failed(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// PDFs: one on this machine loads from disk in the page view, so a
    /// (re)load only moves the page's key on; another device's is fetched as
    /// bytes first.
    fn load_pdf(&mut self, path: String, cx: &mut Context<Self>) {
        let local = self.is_local();
        let remote = if local {
            None
        } else {
            self.engine_and_context(cx)
        };
        let generation = self.generation;
        let Some(file) = self.open_files.get_mut(&path) else {
            return;
        };
        file.preview_gen += 1;
        let Some((engine, context)) = remote else {
            cx.notify();
            return;
        };
        file.media = Media::Loading {
            received: 0,
            total: 0,
        };
        let params = context.params(&path);
        let task_path = path.clone();
        file._media_task = Some(cx.spawn(async move |this, cx| {
            let progress_this = this.clone();
            let mut progress_cx = cx.clone();
            let progress_path = task_path.clone();
            let read = preview::read_bytes(
                &engine,
                params,
                preview::PDF_LIMIT,
                false,
                move |received, total| {
                    progress_this
                        .update(&mut progress_cx, |this, cx| {
                            if let Some(file) = this.open_files.get_mut(&progress_path) {
                                file.media = Media::Loading { received, total };
                                cx.notify();
                            }
                        })
                        .ok();
                },
            )
            .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let Some(file) = this.open_files.get_mut(&task_path) else {
                    return;
                };
                match read {
                    Ok((bytes, total)) => {
                        file.bytes = total;
                        file.media = Media::Pdf(Arc::new(bytes));
                    }
                    Err(error) => file.media = Media::Failed(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Track decoded images; past [`MAX_IMAGES`] the oldest hidden one is
    /// released (it reloads if shown again).
    fn note_image(&mut self, path: String) {
        self.image_order.retain(|p| *p != path);
        self.image_order.push(path);
        while self.image_order.len() > MAX_IMAGES {
            let Some(ix) = self
                .image_order
                .iter()
                .position(|p| self.selected.as_deref() != Some(p.as_str()))
            else {
                break;
            };
            let oldest = self.image_order.remove(ix);
            if let Some(file) = self.open_files.get_mut(&oldest)
                && let Media::Image { image, .. } = std::mem::take(&mut file.media)
            {
                self.evicted.push(image);
            }
        }
    }

    /// Binaries: fetch the head of the file for the hex view.
    fn show_hex(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let Some(file) = self.open_files.get_mut(&path) else {
            return;
        };
        file.hex = true;
        if matches!(file.media, Media::Hex { .. }) {
            cx.notify();
            return;
        }
        file.media = Media::Loading {
            received: 0,
            total: 0,
        };
        let params = context.params(&path);
        let task_path = path.clone();
        file._media_task = Some(cx.spawn(async move |this, cx| {
            let read =
                preview::read_bytes(&engine, params, preview::HEX_LIMIT as u64, true, |_, _| {})
                    .await;
            let media = match read {
                Ok((bytes, total)) => Media::Hex {
                    bytes: Arc::new(bytes),
                    total,
                },
                Err(error) => Media::Failed(error),
            };
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if let Some(file) = this.open_files.get_mut(&task_path) {
                    file.media = media;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// The header's refresh: re-list every expanded folder and re-read the
    /// shown file (replacing unsaved edits — the button says so).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut paths: Vec<String> = self
            .dirs
            .keys()
            .filter(|p| p.is_empty() || self.expanded.contains(*p))
            .cloned()
            .collect();
        paths.sort();
        for path in paths {
            self.load_dir(path, cx);
        }
        if let Some(path) = self.selected.clone() {
            self.reload_open(&path, true, cx);
        }
    }

    /// Re-read an open file (`force` also drops unsaved edits).
    fn reload_open(&mut self, path: &str, force: bool, cx: &mut Context<Self>) {
        let Some(file) = self.open_files.get_mut(path) else {
            return;
        };
        if file.dirty && !force {
            return;
        }
        if force {
            file.dirty = false;
            file.clean = None;
        }
        match file.kind {
            FileKind::Image(_) => self.load_image(path.to_string(), cx),
            FileKind::Pdf => self.load_pdf(path.to_string(), cx),
            _ => {
                if file.hex {
                    file.media = Media::Idle;
                    self.show_hex(path.to_string(), cx);
                }
                self.load_file(path.to_string(), cx);
            }
        }
    }

    fn toggle_tree(&mut self, cx: &mut Context<Self>) {
        self.tree_visible = !self.tree_visible;
        cx.notify();
    }

    /// The header's Source/Preview toggle (Markdown, SVG, HTML). Switching to
    /// the source focuses the editor so typing works at once.
    fn set_preview(&mut self, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let Some(file) = self.open_files.get_mut(&path) else {
            return;
        };
        if file.preview == preview {
            return;
        }
        file.preview = preview;
        if !preview {
            let handle = file.editor.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn set_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        if let Some(file) = self
            .selected
            .as_deref()
            .and_then(|p| self.open_files.get_mut(p))
        {
            file.zoom = zoom;
            cx.notify();
        }
    }

    fn set_hex(&mut self, hex: bool, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        if hex {
            self.show_hex(path, cx);
        } else if let Some(file) = self.open_files.get_mut(&path) {
            file.hex = false;
            cx.notify();
        }
    }

    /// Re-parse the preview when it is shown and the text moved on; kicks a
    /// background highlight of its fenced code (paint-only when it lands).
    /// SVG previews re-render from the current text; HTML ones reload it.
    fn ensure_preview(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(file) = self.open_files.get_mut(path) else {
            return;
        };
        if !file.preview || !file.preview_stale || file.clean.is_none() {
            return;
        }
        if file.kind == FileKind::Html {
            file.preview_stale = false;
            file.preview_gen += 1;
            return;
        }
        if file.kind == FileKind::Svg {
            file.preview_stale = false;
            let (bytes, dims) = preview::svg_preview_bytes(file.editor.read(cx).content());
            file.svg_dims = dims.map(|(w, h)| (w.round() as u32, h.round() as u32));
            if let Some(old) = file
                .svg
                .replace(Arc::new(Image::from_bytes(gpui::ImageFormat::Svg, bytes)))
            {
                self.evicted.push(old);
            }
            return;
        }
        let tree = Arc::new(parse_full(file.editor.read(cx).content()));
        file.preview_stale = false;
        file.preview_gen += 1;
        let generation = file.preview_gen;
        let fenced: Vec<(usize, LanguageId, String)> = tree
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(ix, top)| match &top.block {
                Block::CodeBlock { language, code, .. } => {
                    crate::markdown::diff_fence::highlight_input(language.as_deref(), code)
                        .filter(|(lang, _)| cypher_syntax::supports_language(*lang))
                        .map(|(lang, source)| (ix, lang, source.into_owned()))
                }
                _ => None,
            })
            .collect();
        file.preview_tree = Some(tree);
        file.preview_highlights.clear();
        file._preview_task = None;
        if fenced.is_empty() {
            return;
        }
        let task_path = path.to_string();
        file._preview_task = Some(cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move {
                    fenced
                        .into_iter()
                        .filter_map(|(ix, lang, code)| {
                            cypher_syntax::highlight(cypher_syntax::HighlightRequest {
                                source: &code,
                                path: None,
                                fence_tag: Some(fence_tag(lang)),
                            })
                            .ok()
                            .map(|doc| (ix, Arc::new(doc)))
                        })
                        .collect::<HashMap<_, _>>()
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(file) = this.open_files.get_mut(&task_path)
                    && file.preview_gen == generation
                {
                    file.preview_highlights = highlights;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn wide(&self) -> bool {
        self.pane_width.get() >= SPLIT_MIN_WIDTH
    }

    /// The tree column's laid-out width in a wide pane.
    fn tree_column_width(&self) -> f32 {
        clamp_tree_width(self.tree_width, self.pane_width.get())
    }

    fn set_tree_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = clamp_tree_width(width, self.pane_width.get());
        if width != self.tree_width {
            self.tree_width = width;
            LAST_TREE_WIDTH.store(width.to_bits(), std::sync::atomic::Ordering::Relaxed);
            cx.notify();
        }
    }

    /// The seam between the tree and the file: drag to resize, double-click
    /// for the default width. It floats over the column's border (no layout
    /// width), its hairline lighting up under the pointer.
    fn render_seam(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let panel = cx.entity_id();
        let line = theme.border_strong;
        div()
            .id("files-tree-seam")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(self.tree_column_width() - 3.0))
            .w(px(6.0))
            .cursor_col_resize()
            .group("files-tree-seam")
            .flex()
            .justify_center()
            .on_drag(TreeResize(panel), |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| ResizeGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    if event.click_count == 2 {
                        this.set_tree_width(TREE_WIDTH, cx);
                    }
                }),
            )
            .child(
                div()
                    .w(px(2.0))
                    .h_full()
                    .group_hover("files-tree-seam", move |s| s.bg(line)),
            )
            .into_any_element()
    }

    // ---- entries moving under the tree ----

    /// Everything keyed by a path follows a renamed/moved entry: open files
    /// (unsaved edits included), listings, expansion, selection, clipboard.
    fn entry_moved(&mut self, from: &str, to: &str, cx: &mut Context<Self>) {
        let rekey = |path: &String| tree::rebase(path, from, to);
        let moved: Vec<(String, String)> = self
            .open_files
            .keys()
            .filter_map(|path| rekey(path).map(|next| (path.clone(), next)))
            .collect();
        for (old, new) in moved {
            if let Some(mut file) = self.open_files.remove(&old) {
                file.editor
                    .update(cx, |editor, cx| editor.set_path(&new, cx));
                let kind = FileKind::for_path(&new);
                if kind != file.kind && !file.kind.media() {
                    file.preview = kind.previews();
                    file.preview_stale = true;
                    file.kind = kind;
                }
                self.open_files.insert(new, file);
            }
        }
        if let Some(next) = self.selected.as_ref().and_then(rekey) {
            self.selected = Some(next);
        }
        let dirs: Vec<String> = self
            .dirs
            .keys()
            .filter(|p| rekey(p).is_some())
            .cloned()
            .collect();
        for old in dirs {
            if let (Some(state), Some(new)) = (self.dirs.remove(&old), rekey(&old)) {
                self.dirs.insert(new, state);
            }
        }
        self.expanded = self
            .expanded
            .iter()
            .map(|p| rekey(p).unwrap_or_else(|| p.clone()))
            .collect();
        for path in &mut self.image_order {
            if let Some(next) = tree::rebase(path, from, to) {
                *path = next;
            }
        }
        if let Some(clip) = self.clipboard.as_mut() {
            for (path, _) in &mut clip.items {
                if let Some(next) = tree::rebase(path, from, to) {
                    *path = next;
                }
            }
        }
        self.selection.rebase(from, to);
    }

    /// Drop everything under a deleted entry (its editors included).
    fn entry_removed(&mut self, path: &str) {
        if path.is_empty() {
            return;
        }
        let gone = |p: &str| tree::within(path, p);
        let files: Vec<String> = self
            .open_files
            .keys()
            .filter(|p| gone(p))
            .cloned()
            .collect();
        for key in files {
            if let Some(file) = self.open_files.remove(&key)
                && let Media::Image { image, .. } = file.media
            {
                self.evicted.push(image);
            }
        }
        if self.selected.as_deref().is_some_and(gone) {
            self.selected = None;
        }
        self.dirs.retain(|p, _| !gone(p));
        self.expanded.retain(|p| !gone(p));
        self.image_order.retain(|p| !gone(p));
        self.selection.items.retain(|p| !gone(p));
        if let Some(clip) = self.clipboard.as_mut() {
            clip.items.retain(|(p, _)| !gone(p));
        }
    }

    // ---- mouse & keyboard ----

    fn click_row(
        &mut self,
        path: &str,
        is_dir: bool,
        modifiers: gpui::Modifiers,
        double: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.tree_focus, cx);
        if modifiers.secondary() {
            self.selection.toggle(path);
        } else if modifiers.shift {
            self.selection.extend_to(&self.rows, path);
        } else {
            self.selection.single(path);
            if is_dir {
                if !double {
                    self.toggle_dir(path.to_string(), cx);
                }
            } else {
                // A single click previews (focus stays in the tree for the
                // keyboard); a double click moves typing into the file.
                self.open_file(path.to_string(), double, Some(window), cx);
            }
        }
        cx.notify();
    }

    fn open_row_menu(
        &mut self,
        path: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.tree_focus, cx);
        if !self.selection.contains(path) {
            self.selection.single(path);
        }
        let targets = self.selection.resolve(&self.rows);
        self.menu.open(menu::MenuState { position, targets });
        cx.notify();
    }

    fn open_background_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.tree_focus, cx);
        self.selection.clear();
        self.menu.open(menu::MenuState {
            position,
            targets: Vec::new(),
        });
        cx.notify();
    }

    /// The tree's keyboard: arrows move/expand, ⇧ extends, ⏎ opens, F2
    /// renames, ⌘⌫ trashes (⌥ for good), ⌘C/⌘X/⌘V/⌘D, ⌘A, ⎋.
    fn tree_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only when the tree itself has focus — never while typing in the
        // inline name field.
        if !self.tree_focus.is_focused(window) || self.edit.is_some() {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let cmd = modifiers.secondary();
        let picked = self.selection.resolve(&self.rows);
        let cursor_row = self
            .selection
            .cursor
            .as_deref()
            .and_then(|c| self.rows.iter().find(|r| r.path == c && r.is_entry()))
            .cloned();
        let target_dir = match picked.as_slice() {
            [(path, true)] => path.clone(),
            [(path, false)] => parent_of(path).to_string(),
            _ => String::new(),
        };
        let handled = match (keystroke.key.as_str(), cmd) {
            ("up" | "down", false) => {
                let delta = if keystroke.key == "up" { -1 } else { 1 };
                if let Some(next) = tree::step(&self.rows, self.selection.cursor.as_deref(), delta)
                {
                    if modifiers.shift {
                        self.selection.extend_to(&self.rows, &next);
                    } else {
                        self.selection.single(&next);
                    }
                    self.scroll_to_path(&next);
                }
                true
            }
            ("home" | "end", _) | ("up" | "down", true) => {
                // No cursor: step(+1) lands on the first entry, step(-1) on
                // the last.
                let first = matches!(keystroke.key.as_str(), "home" | "up");
                if let Some(next) = tree::step(&self.rows, None, if first { 1 } else { -1 }) {
                    self.selection.single(&next);
                    self.scroll_to_path(&next);
                }
                true
            }
            ("left", false) => {
                if let Some(row) = cursor_row {
                    if row.is_dir && row.expanded {
                        self.set_expanded(&row.path, false, cx);
                    } else if !parent_of(&row.path).is_empty() {
                        let parent = parent_of(&row.path).to_string();
                        self.selection.single(&parent);
                        self.scroll_to_path(&parent);
                    }
                }
                true
            }
            ("right", false) => {
                if let Some(row) = cursor_row
                    && row.is_dir
                {
                    if row.expanded {
                        if let Some(next) = tree::step(&self.rows, Some(&row.path), 1)
                            && parent_of(&next) == row.path
                        {
                            self.selection.single(&next);
                            self.scroll_to_path(&next);
                        }
                    } else {
                        self.set_expanded(&row.path, true, cx);
                    }
                }
                true
            }
            ("enter", false) => {
                if let Some(row) = cursor_row {
                    if row.is_dir {
                        self.toggle_dir(row.path.clone(), cx);
                    } else {
                        self.open_file(row.path.clone(), true, Some(window), cx);
                    }
                }
                true
            }
            ("f2", _) => {
                if let [(path, is_dir)] = picked.as_slice() {
                    self.begin_rename(path.clone(), *is_dir, window, cx);
                }
                true
            }
            ("backspace", true) | ("delete", _) => {
                self.request_delete(picked, modifiers.alt, window, cx);
                true
            }
            ("c", true) => {
                self.copy_entries(picked, false, cx);
                true
            }
            ("x", true) => {
                self.copy_entries(picked, true, cx);
                true
            }
            ("v", true) => {
                self.paste(target_dir, cx);
                true
            }
            ("d", true) => {
                self.duplicate(picked, cx);
                true
            }
            ("a", true) => {
                self.selection.items = self
                    .rows
                    .iter()
                    .filter(|r| r.is_entry())
                    .map(|r| r.path.clone())
                    .collect();
                true
            }
            ("escape", false) => {
                if self.clipboard.as_ref().is_some_and(|c| c.cut) {
                    self.clipboard = None;
                } else {
                    self.selection.clear();
                }
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn scroll_to_path(&self, path: &str) {
        if let Some(ix) = self.rows.iter().position(|row| row.path == path) {
            self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
    }

    // ---- go to file ----

    fn on_filter_edited(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.input.read(cx).text().trim().to_string();
        if query == self.filter.query {
            return;
        }
        self.filter.query = query.clone();
        self.filter.active = 0;
        if query.is_empty() {
            self.filter.results = Loadable::Idle;
            self.filter.task = None;
            cx.notify();
            return;
        }
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        if !matches!(self.filter.results, Loadable::Ready(_)) {
            self.filter.results = Loadable::Loading;
        }
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), context.chat_id.clone().into());
        params.insert("query".into(), query.clone().into());
        context.address(&mut params);
        self.filter.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(120))
                .await;
            let result = call_with_retry(
                &engine,
                methods::SEARCH_FILES,
                serde_json::Value::Object(params),
                cx,
            )
            .await
            .and_then(|value| {
                serde_json::from_value::<Vec<FileSearchMatch>>(value)
                    .map_err(|e| RpcError::Failed(e.to_string()))
            });
            this.update(cx, |this, cx| {
                if this.filter.query != query {
                    return;
                }
                this.filter.results = match result {
                    Ok(matches) => Loadable::Ready(matches),
                    Err(err) => Loadable::Error(rpc_message(&err).to_string()),
                };
                this.filter.active = 0;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn clear_filter(&mut self, cx: &mut Context<Self>) {
        if self.filter.query.is_empty() && self.filter.input.read(cx).text().is_empty() {
            return;
        }
        self.filter.query.clear();
        self.filter.results = Loadable::Idle;
        self.filter.task = None;
        self.filter
            .input
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        cx.notify();
    }

    fn filter_matches(&self) -> &[FileSearchMatch] {
        self.filter.results.ready().map_or(&[], Vec::as_slice)
    }

    /// Open a go-to-file result: a file opens (typing moves into it), a
    /// folder is revealed and expanded in the tree.
    fn accept_filter(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.filter_matches().get(ix).cloned() else {
            return;
        };
        let path = hit.path.trim_end_matches('/').to_string();
        self.clear_filter(cx);
        self.reveal_in_tree(path.clone(), cx);
        // The tree takes the keyboard; a text file then hands it on to its
        // editor (previews, images and binaries leave it in the tree).
        window.focus(&self.tree_focus, cx);
        if hit.is_dir {
            self.set_expanded(&path, true, cx);
        } else {
            self.open_file(path, true, Some(window), cx);
        }
    }

    fn filter_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.filter_matches().len();
        match event.keystroke.key.as_str() {
            "escape" => {
                self.clear_filter(cx);
                window.focus(&self.tree_focus, cx);
            }
            "down" if count > 0 => {
                self.filter.active = (self.filter.active + 1).min(count - 1);
                self.filter
                    .scroll
                    .scroll_to_item(self.filter.active, ScrollStrategy::Nearest);
            }
            "up" if count > 0 => {
                self.filter.active = self.filter.active.saturating_sub(1);
                self.filter
                    .scroll
                    .scroll_to_item(self.filter.active, ScrollStrategy::Nearest);
            }
            "enter" => self.accept_filter(self.filter.active, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ---- checkout diff stream ----

    /// Watch the host's checkout diffs (like the diff pane): git marks for
    /// the tree and a signal that the agent changed files.
    fn ensure_watch(&mut self, cx: &mut Context<Self>) {
        let Some(context) = &self.context else {
            return;
        };
        let target = context.target.clone();
        if self.watch_task.is_some() && self.watch_target.as_ref() == Some(&target) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.diffs.clear();
        self.watch_target = Some(target.clone());
        self.watch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let mut params = serde_json::Map::new();
                if let Some(target) = &target {
                    params.insert("targetDeviceId".into(), target.clone().into());
                }
                if let Ok(mut rx) = engine
                    .client()
                    .subscribe(
                        methods::WATCH_CHECKOUT_DIFFS,
                        serde_json::Value::Object(params),
                    )
                    .await
                {
                    while let Some(value) = rx.recv().await {
                        let alive = this.update(cx, |this, cx| {
                            if crate::changes::apply_diff_frame(&mut this.diffs, value) {
                                this.apply_diffs(cx);
                            }
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(3))
                    .await;
            }
        }));
    }

    /// Fold the selected chat's diff into git marks; a changed diff means
    /// files changed on disk — refresh quietly.
    fn apply_diffs(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(chat) = state.selected_chat_row() else {
            return;
        };
        // A workspace's own chat: the shown root's checkout.
        let at_root = state
            .workspace_chat_selected()
            .then(|| {
                state
                    .files_root()
                    .map(|root| state.chat_at_root(chat, root))
            })
            .flatten();
        let diff = crate::changes::resolve_diff(&self.diffs, at_root.as_ref().unwrap_or(chat));
        let checksum = diff.map(|d| d.checksum.clone());
        if checksum == self.diff_checksum {
            return;
        }
        let paths: HashSet<String> = diff
            .map(|d| d.files.iter().map(|f| f.path.clone()).collect())
            .unwrap_or_default();
        let changed: HashSet<String> = paths.union(&self.diff_paths).cloned().collect();
        let first = self.diff_checksum.is_none();
        self.git = GitMarks::from_diff(diff);
        self.diff_checksum = checksum;
        self.diff_paths = paths;
        if !first {
            self.schedule_soft_refresh(changed, cx);
        }
        cx.notify();
    }

    /// Debounced: re-list the visible folders and re-read clean open files
    /// that changed (others reload when next shown).
    fn schedule_soft_refresh(&mut self, changed: HashSet<String>, cx: &mut Context<Self>) {
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            this.update(cx, |this, cx| {
                let dirs: Vec<String> = this
                    .dirs
                    .iter()
                    .filter(|(p, d)| d.loaded && (p.is_empty() || this.expanded.contains(*p)))
                    .map(|(p, _)| p.clone())
                    .collect();
                for dir in dirs {
                    this.load_dir(dir, cx);
                }
                let open: Vec<String> = this
                    .open_files
                    .keys()
                    .filter(|p| changed.contains(*p))
                    .cloned()
                    .collect();
                for path in open {
                    if this.selected.as_deref() == Some(path.as_str()) {
                        this.reload_open(&path, false, cx);
                    } else if let Some(file) = this.open_files.get_mut(&path) {
                        file.stale = true;
                    }
                }
            })
            .ok();
        }));
    }

    // ---- rendering ----

    /// The surface's second header row (the shell hosts it above the body):
    /// tree toggle · path · view toggles · save state · refresh.
    pub fn render_header_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let selected = self.selected.clone();
        let file = selected.as_deref().and_then(|p| self.open_files.get(p));
        let dirty = file.is_some_and(|f| f.dirty);
        let saving = file.is_some_and(|f| f.saving);
        let kind = file.map(|f| f.kind);
        let binary = file.is_some_and(|f| f.binary);
        let image = matches!(kind, Some(FileKind::Image(_)));
        let media = kind.is_some_and(FileKind::media);
        let read_only = file.is_some_and(|f| f.editor.read(cx).read_only()) && !media && !binary;
        let error = file.and_then(|f| f.error.clone());
        let preview = file.map(|f| f.preview);
        let zoom = file.map(|f| f.zoom);
        let hex = file.is_some_and(|f| f.hex);
        let shows_text =
            file.is_some_and(|f| !f.binary && !f.kind.media() && !(f.preview && f.kind.previews()));
        let position = shows_text
            .then(|| file.map(|f| f.editor.read(cx).caret_position()))
            .flatten();
        let tree_on = self.tree_visible;
        let wide = self.wide();
        let tree_shown = if wide {
            tree_on
        } else {
            tree_on || selected.is_none()
        };
        // No "Files" title (user request) — the row names the open file
        // only.
        let path_label: SharedString = selected.clone().unwrap_or_default().into();
        // A workspace's own chat with more than one folder: which one.
        let picker = {
            let state = self.state.read(cx);
            match (state.file_roots(), state.files_root()) {
                (crate::state::FileRoots::Ready(list), Some(shown)) if list.len() > 1 => {
                    Some((list.to_vec(), shown.clone()))
                }
                _ => None,
            }
        }
        .map(|(choices, shown)| {
            let mounted = root_picker::mounted(&self.root_menu);
            root_picker::render(
                "files-root-picker",
                &choices,
                &shown,
                &self.state.clone(),
                |this: &mut Self| &mut this.root_menu,
                mounted,
                &theme,
                cx,
            )
        });

        let mut row = div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                small_icon_button(
                    "files-toggle-tree",
                    icons::SIDEBAR_MINIMALISTIC_LEFT,
                    &theme,
                    tree_on || (!wide && selected.is_none()),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_tree(cx))),
            )
            .children(picker)
            .when(tree_shown && self.context.is_some(), |el| {
                el.child(
                    small_icon_button("files-new-file", icons::DOCUMENT_ADD, &theme, false)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let dir = this.folder_for_new();
                            this.begin_create(dir, false, window, cx);
                        })),
                )
                .child(
                    small_icon_button("files-new-folder", icons::FOLDER_ADD, &theme, false)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let dir = this.folder_for_new();
                            this.begin_create(dir, true, window, cx);
                        })),
                )
                .child(
                    small_icon_button("files-collapse-all", icons::FOLD_VERTICAL, &theme, false)
                        .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(if selected.is_some() {
                        theme.text.opacity(0.85)
                    } else {
                        theme.text_muted
                    })
                    .child(path_label),
            );
        if let Some(error) = error {
            row = row.child(
                div()
                    .flex_none()
                    .max_w(px(220.0))
                    .truncate()
                    .text_size(px(11.0))
                    .text_color(theme.danger)
                    .child(error),
            );
        }
        if kind.is_some_and(FileKind::previews) && !binary {
            let on = preview.unwrap_or(false);
            row = row.child(segmented(
                &theme,
                [
                    chip("files-md-source", "Source", !on, &theme).on_click(
                        cx.listener(|this, _, window, cx| this.set_preview(false, window, cx)),
                    ),
                    chip("files-md-preview", "Preview", on, &theme).on_click(
                        cx.listener(|this, _, window, cx| this.set_preview(true, window, cx)),
                    ),
                ],
            ));
        }
        if image {
            let fit = zoom != Some(Zoom::Actual);
            row = row.child(segmented(
                &theme,
                [
                    chip("files-zoom-fit", "Fit", fit, &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.set_zoom(Zoom::Fit, cx))),
                    chip("files-zoom-actual", "100%", !fit, &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.set_zoom(Zoom::Actual, cx))),
                ],
            ));
        }
        if binary {
            row = row.child(segmented(
                &theme,
                [
                    chip("files-bin-info", "Info", !hex, &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.set_hex(false, cx))),
                    chip("files-bin-hex", "Hex", hex, &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.set_hex(true, cx))),
                ],
            ));
        }
        if let Some((line, column)) = position {
            row = row.child(
                div()
                    .flex_none()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!("{line}:{column}"))),
            );
        }
        if read_only && selected.is_some() {
            row = row.child(
                div()
                    .flex_none()
                    .px(px(6.0))
                    .h(px(20.0))
                    .rounded(px(5.0))
                    .bg(crate::kit::theme::ink(0.05))
                    .flex()
                    .items_center()
                    .text_size(px(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from("Read-only")),
            );
        } else if dirty || saving {
            let label = if saving { "Saving…" } else { "Save" };
            let save_path = selected.clone();
            row = row.child(
                div()
                    .id("files-save")
                    .flex_none()
                    .h(px(22.0))
                    .px(px(9.0))
                    .rounded(px(6.0))
                    .bg(theme.accent)
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.bg)
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(saving, |el| el.opacity(0.6))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(path) = save_path.clone() {
                            this.save_file(path, cx);
                        }
                    }))
                    .child(SharedString::from(label)),
            );
        }
        row = row.child(
            small_icon_button("files-refresh", icons::REFRESH, &theme, false)
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
        );
        row.into_any_element()
    }

    /// The tree column: go-to-file + new entry toolbar, the tree (or the
    /// filter's results), and the status footer.
    fn render_tree_column(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if let Some(error) = &self.context_error {
            return centered_note(error.clone(), &theme);
        }
        let body = if self.filter.query.is_empty() {
            self.render_tree(window, cx)
        } else {
            self.render_filter_results(cx)
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(&theme, cx))
            .child(div().flex_1().min_h_0().child(body))
            .children(self.render_status(&theme, cx))
            .into_any_element()
    }

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(ROW_INSET))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(24.0))
                    .px(px(6.0))
                    .rounded(px(6.0))
                    .bg(crate::kit::theme::ink(0.04))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .on_key_down(cx.listener(Self::filter_key_down))
                    .child(
                        icon(icons::MAGNIFER)
                            .size(px(12.0))
                            .flex_none()
                            .text_color(theme.text_faint),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(self.filter.input.clone()),
                    )
                    .when(!self.filter.query.is_empty(), |el| {
                        el.child(
                            div()
                                .id("files-filter-clear")
                                .flex_none()
                                .cursor_pointer()
                                .child(
                                    icon(icons::CLOSE)
                                        .size(px(10.0))
                                        .text_color(theme.text_faint),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.clear_filter(cx);
                                    window.focus(&this.tree_focus, cx);
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if let Some(edit) = self.edit.as_mut()
            && std::mem::take(&mut edit.focus_pending)
        {
            let handle = edit.input.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        }
        let ctx = tree::RowContext {
            panel: cx.entity(),
            panel_id: cx.entity_id(),
            open: self.selected.clone(),
            picked: self.selection.resolve(&self.rows),
            selection: self.selection.clone(),
            dirty: self
                .open_files
                .iter()
                .filter(|(_, f)| f.dirty)
                .map(|(p, _)| p.clone())
                .collect(),
            cut: self
                .clipboard
                .as_ref()
                .filter(|c| c.cut)
                .map(|c| c.items.iter().map(|(p, _)| p.clone()).collect())
                .unwrap_or_default(),
            git: self.git.clone(),
            edit: self
                .edit
                .as_ref()
                .map(|e| (e.target.clone(), e.input.clone(), e.error.clone())),
            tree_focused: self.tree_focus.contains_focused(window, cx),
        };
        let count = self.rows.len();
        let rows = self.rows.clone();
        let row_theme = theme.clone();
        let list = uniform_list("files-tree", count, move |range, _window, _cx| {
            range
                .filter_map(|ix| {
                    rows.get(ix)
                        .map(|row| tree::render_row(ix, row, &ctx, &row_theme))
                })
                .collect()
        })
        .track_scroll(&self.tree_scroll)
        .size_full()
        .py(px(2.0))
        .px(px(ROW_INSET));
        let panel_id = cx.entity_id();
        div()
            .id("files-tree-area")
            .size_full()
            .track_focus(&self.tree_focus)
            .on_key_down(cx.listener(Self::tree_key_down))
            // The empty area is the project root: click clears the
            // selection, right-click offers root actions, drops land there.
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.tree_focus, cx);
                this.selection.clear();
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.open_background_menu(event.position, window, cx)
                }),
            )
            .on_drop(cx.listener(move |this, drag: &tree::FileDrag, window, cx| {
                if drag.panel == panel_id && drag.fits("") {
                    let copy = window.modifiers().alt;
                    this.transfer(drag.items.clone(), String::new(), copy, cx);
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.import(paths.paths().to_vec(), String::new(), cx);
            }))
            .child(list)
            .into_any_element()
    }

    fn render_filter_results(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        match &self.filter.results {
            Loadable::Idle | Loadable::Loading => {
                return centered_note("Searching…".into(), &theme);
            }
            Loadable::Error(error) => return centered_note(error.clone().into(), &theme),
            Loadable::Ready(matches) if matches.is_empty() => {
                return centered_note("No matching files.".into(), &theme);
            }
            Loadable::Ready(_) => {}
        }
        let matches: Vec<FileSearchMatch> = self.filter_matches().to_vec();
        let active = self.filter.active;
        let entity = cx.entity();
        uniform_list("files-filter", matches.len(), move |range, _window, _cx| {
            range
                .filter_map(|ix| {
                    let hit = matches.get(ix)?;
                    let path = hit.path.trim_end_matches('/');
                    let name = tree::name_of(path).to_string();
                    let parent = parent_of(path).to_string();
                    let glyph = if hit.is_dir {
                        icons::FOLDER
                    } else {
                        preview::file_icon(&name)
                    };
                    let entity = entity.clone();
                    Some(
                        div()
                            .id(("files-filter-row", ix))
                            .w_full()
                            .h(px(ROW_HEIGHT))
                            .px(px(8.0))
                            .rounded(px(ROW_RADIUS))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .cursor_pointer()
                            .when(ix == active, |el| el.bg(crate::kit::theme::wash(0.10)))
                            .hover(|s| s.bg(crate::kit::theme::wash(0.06)))
                            .on_click(move |_, window, cx| {
                                entity.update(cx, |this, cx| this.accept_filter(ix, window, cx));
                            })
                            .child(
                                icon(glyph)
                                    .size(px(12.0))
                                    .flex_none()
                                    .text_color(theme.text_faint),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(160.0))
                                    .truncate()
                                    .text_size(px(12.0))
                                    .text_color(theme.text)
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(parent)),
                            )
                            .into_any_element(),
                    )
                })
                .collect()
        })
        .track_scroll(&self.filter.scroll)
        .size_full()
        .py(px(2.0))
        .px(px(ROW_INSET))
        .into_any_element()
    }

    fn render_status(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (seq, error, text) = {
            let status = self.status.as_ref()?;
            (status.seq, status.error, status.text.clone())
        };
        if !error && !text.ends_with('…') && self.status_fade != seq {
            self.status_fade = seq;
            // Results fade on their own; progress and errors stay.
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(4))
                    .await;
                this.update(cx, |this, cx| {
                    if this.status.as_ref().is_some_and(|s| s.seq == seq) {
                        this.status = None;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        Some(
            div()
                .flex_none()
                .min_h(px(26.0))
                .px(px(10.0))
                .py(px(4.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.0))
                        .line_height(px(15.0))
                        .text_color(if error {
                            theme.danger
                        } else {
                            theme.text_muted
                        })
                        .child(text),
                )
                .when(error, |el| {
                    el.child(
                        div()
                            .id("files-status-dismiss")
                            .flex_none()
                            .cursor_pointer()
                            .child(
                                icon(icons::CLOSE)
                                    .size(px(10.0))
                                    .text_color(theme.text_faint),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.status = None;
                                cx.notify();
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_editor_side(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(path) = self.selected.clone() else {
            return centered_note("Select a file to view it.".into(), &theme);
        };
        self.ensure_preview(&path, cx);
        if self.focus_editor_pending
            && let Some(file) = self.open_files.get(&path)
            && (file.clean.is_some() || file.error.is_some() || file.binary)
        {
            self.focus_editor_pending = false;
            // Only into a shown editor — never a preview, image, PDF or binary.
            let editing =
                !file.binary && !file.kind.media() && !(file.preview && file.kind.previews());
            if editing {
                let handle = file.editor.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        }
        let local = self.is_local();
        let modified = self.entry_meta(&path).and_then(|e| e.modified_ms);
        // An HTML preview of a file on this machine reads it from disk (or
        // resolves unsaved text beside it), so its relative files load. A PDF
        // there loads from disk too.
        let on_disk = local.then(|| (self.absolute_path(&path), self.absolute_path("")));
        let header_right = self.header_right;
        let menu_open = self.menu.is_open() || self.confirm.is_some();
        let generation = self.generation;
        let panel = cx.entity_id();
        let Some(file) = self.open_files.get(&path) else {
            return centered_note("Select a file to view it.".into(), &theme);
        };
        if let FileKind::Image(_) = file.kind {
            return match &file.media {
                Media::Image { image, dims } => {
                    preview::render_image(&path, image, *dims, file.bytes, file.zoom, &theme)
                }
                Media::Loading { received, total } if *total > 0 => centered_note(
                    format!(
                        "Loading… {} of {}",
                        human_bytes(*received),
                        human_bytes(*total)
                    )
                    .into(),
                    &theme,
                ),
                Media::Failed(error) => self.render_info(
                    &path,
                    file.bytes,
                    modified,
                    Some(error.clone()),
                    local,
                    &theme,
                    cx,
                ),
                _ => centered_note("Loading…".into(), &theme),
            };
        }
        if file.kind == FileKind::Pdf {
            let source = match (&on_disk, &file.media) {
                (Some((absolute, root)), _) => web_view::Source::File {
                    path: absolute,
                    root,
                },
                (None, Media::Pdf(bytes)) => web_view::Source::Data {
                    bytes: bytes.as_slice(),
                    mime: "application/pdf",
                },
                (None, Media::Loading { received, total }) if *total > 0 => {
                    return centered_note(
                        format!(
                            "Loading… {} of {}",
                            human_bytes(*received),
                            human_bytes(*total)
                        )
                        .into(),
                        &theme,
                    );
                }
                (None, Media::Failed(error)) => {
                    return self.render_info(
                        &path,
                        file.bytes,
                        modified,
                        Some(error.clone()),
                        local,
                        &theme,
                        cx,
                    );
                }
                _ => return centered_note("Loading…".into(), &theme),
            };
            let key = {
                use std::hash::{Hash as _, Hasher as _};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (
                    "pdf",
                    generation,
                    &path,
                    file.preview_gen,
                    on_disk.is_some(),
                )
                    .hash(&mut hasher);
                hasher.finish()
            };
            let web = self.web.get_or_insert_with(web_view::WebView::new);
            web.load(key, source);
            return preview::render_page(
                web.element(format!("files-web:{panel}").into(), menu_open),
                header_right,
            );
        }
        if file.loading && file.clean.is_none() {
            return centered_note("Loading…".into(), &theme);
        }
        if let Some(error) = &file.error
            && file.clean.is_none()
        {
            return centered_note(error.clone(), &theme);
        }
        if file.binary {
            if file.hex {
                return match &file.media {
                    Media::Hex { bytes, total } => {
                        let width = if self.wide() && self.tree_visible {
                            self.pane_width.get() - self.tree_column_width()
                        } else {
                            self.pane_width.get() - self.header_right
                        };
                        preview::render_hex(
                            bytes.clone(),
                            *total,
                            preview::hex_per_row(width),
                            &file.hex_scroll,
                            &theme,
                        )
                    }
                    Media::Failed(error) => centered_note(error.clone(), &theme),
                    _ => centered_note("Loading…".into(), &theme),
                };
            }
            let bytes = file.bytes;
            return self.render_info(&path, bytes, modified, None, local, &theme, cx);
        }
        let notice: Option<SharedString> = if file.truncated {
            Some(
                format!(
                    "Showing the first 256 KiB of {} — read-only.",
                    human_bytes(file.bytes)
                )
                .into(),
            )
        } else {
            None
        };
        let body: AnyElement = match (file.preview, file.kind, &file.preview_tree, &file.svg) {
            (true, FileKind::Svg, _, Some(svg)) => preview::render_image(
                &path,
                svg,
                file.svg_dims,
                file.editor.read(cx).content().len() as u64,
                Zoom::Fit,
                &theme,
            ),
            (true, FileKind::Markdown, Some(tree), _) => {
                let mut options = render::RenderOptions::settled(format!("files-md:{path}").into());
                options.scope = self.selection_scope;
                let entity = cx.weak_entity();
                options.diagram_toggled = Some(Rc::new(move |_, cx| {
                    entity.update(cx, |_, cx| cx.notify()).ok();
                }));
                let highlights = file.preview_highlights.clone();
                for top in &tree.blocks {
                    crate::markdown::mermaid::request_block(
                        &top.block,
                        &theme,
                        window.scale_factor(),
                        cx,
                        |_, cx| cx.notify(),
                    );
                }
                let rendered = render::render_tree(tree, &options, &theme, window, &|ix| {
                    highlights.get(&ix).cloned()
                });
                div()
                    .id(SharedString::from(format!("files-md-preview:{path}")))
                    .size_full()
                    .overflow_y_scroll()
                    .px(px(20.0))
                    .py(px(16.0))
                    .font_family(theme.font_sans.clone())
                    .text_size(px(theme.markdown.body_size))
                    .line_height(px(theme.markdown.body_line_height))
                    .text_color(theme.text)
                    // FIRST child ⇒ paints first: without the per-frame reset
                    // the selection registry grows every repaint, so drags
                    // hit-test stale geometry and slow to a halt.
                    .child(render::selection_frame_reset(self.selection_scope))
                    .child(div().w_full().max_w(px(760.0)).child(rendered))
                    .into_any_element()
            }
            (true, FileKind::Html, _, _) => {
                let from_disk = on_disk.is_some() && !file.dirty;
                let key = {
                    use std::hash::{Hash as _, Hasher as _};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    (generation, &path, file.preview_gen, from_disk).hash(&mut hasher);
                    hasher.finish()
                };
                let base = on_disk
                    .as_ref()
                    .map(|(absolute, _)| web_view::base_dir(absolute));
                let source = match &on_disk {
                    Some((absolute, root)) if from_disk => web_view::Source::File {
                        path: absolute,
                        root,
                    },
                    _ => web_view::Source::Text {
                        html: file.editor.read(cx).content(),
                        base: base.as_deref(),
                    },
                };
                let web = self.web.get_or_insert_with(web_view::WebView::new);
                web.load(key, source);
                preview::render_page(
                    web.element(format!("files-web:{panel}").into(), menu_open),
                    header_right,
                )
            }
            _ => file.editor.clone().into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .when_some(notice, |el, notice| {
                el.child(
                    div()
                        .flex_none()
                        .px(px(12.0))
                        .py(px(5.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .child(notice),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// The info card for a binary (or an image that could not load), with
    /// its actions.
    #[allow(clippy::too_many_arguments)]
    fn render_info(
        &self,
        path: &str,
        bytes: u64,
        modified: Option<i64>,
        error: Option<SharedString>,
        local: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut buttons = Vec::new();
        if error.is_none() {
            buttons.push(
                preview::card_button("files-info-hex", "Show Hex", theme)
                    .on_click(cx.listener(|this, _, _, cx| this.set_hex(true, cx)))
                    .into_any_element(),
            );
        }
        if local {
            let open_path = path.to_string();
            buttons.push(
                preview::card_button("files-info-open", "Open with Default App", theme)
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.open_with_default_app(&open_path, cx)
                        }),
                    )
                    .into_any_element(),
            );
            let reveal_path = path.to_string();
            let label = if cfg!(target_os = "macos") {
                "Reveal in Finder"
            } else {
                "Show in Folder"
            };
            buttons.push(
                preview::card_button("files-info-reveal", label, theme)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.reveal_in_finder(&reveal_path, cx)),
                    )
                    .into_any_element(),
            );
        }
        let card = preview::info_card(path, bytes, modified, buttons, theme);
        match error {
            Some(error) => div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_none()
                        .px(px(12.0))
                        .py(px(5.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(px(11.0))
                        .text_color(theme.danger)
                        .child(error),
                )
                .child(div().flex_1().min_h_0().child(card))
                .into_any_element(),
            None => card,
        }
    }

    /// The listing entry for `path` (size, modified time), when its folder
    /// is loaded.
    fn entry_meta(&self, path: &str) -> Option<&WorkspaceFileEntry> {
        let name = tree::name_of(path);
        self.dirs
            .get(parent_of(path))?
            .entries
            .iter()
            .find(|e| e.name == name)
    }
}

/// The fence alias `cypher_syntax` resolves back to `lang` (the transcript's
/// table): highlighting a fenced block goes through the alias path so the
/// parser picks the same injections it would for a chat reply.
pub(crate) fn fence_tag(lang: LanguageId) -> &'static str {
    match lang {
        LanguageId::Rust => "rust",
        LanguageId::JavaScript => "javascript",
        LanguageId::Jsx => "jsx",
        LanguageId::TypeScript => "typescript",
        LanguageId::Tsx => "tsx",
        LanguageId::Python => "python",
        LanguageId::Go => "go",
        LanguageId::Json => "json",
        LanguageId::Jsonc => "jsonc",
        LanguageId::Bash => "bash",
        LanguageId::Toml => "toml",
        LanguageId::Markdown => "markdown",
        LanguageId::Html => "html",
        LanguageId::Css => "css",
        LanguageId::Yaml => "yaml",
        LanguageId::C => "c",
        LanguageId::Cpp => "cpp",
        LanguageId::CSharp => "csharp",
        LanguageId::Java => "java",
        LanguageId::Kotlin => "kotlin",
        LanguageId::Swift => "swift",
        LanguageId::Ruby => "ruby",
        LanguageId::Php => "php",
        LanguageId::Sql => "sql",
        LanguageId::Lua => "lua",
        LanguageId::Dockerfile => "dockerfile",
        LanguageId::Nix => "nix",
        LanguageId::Make => "make",
    }
}

fn chip(
    id: &'static str,
    label: &'static str,
    active: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(20.0))
        .px(px(7.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(active, |el| {
            el.bg(crate::kit::theme::wash(0.12)).text_color(theme.text)
        })
        .when(!active, |el| {
            el.text_color(theme.text_muted)
                .hover(|s| s.bg(crate::kit::theme::wash(0.06)))
        })
        .child(SharedString::from(label))
}

fn segmented<const N: usize>(_theme: &Theme, chips: [gpui::Stateful<gpui::Div>; N]) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .p(px(1.0))
        .rounded(px(6.0))
        .bg(crate::kit::theme::ink(0.04))
        .children(chips)
}

fn centered_note(text: SharedString, theme: &Theme) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(16.0))
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .text_center()
        .child(text)
        .into_any_element()
}

fn small_icon_button(
    id: &'static str,
    icon_path: &'static str,
    theme: &Theme,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .when(active, |el| el.bg(crate::kit::theme::wash(0.10)))
        .hover(|s| s.bg(crate::kit::theme::wash(0.12)))
        .child(icon(icon_path).size(px(13.0)).text_color(theme.text_muted))
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// User-facing text for a failed workspace RPC.
pub(crate) fn rpc_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::Transport(_) | RpcError::Closed => "Could not reach the session's device.".into(),
        // Old engines lack the newer workspace RPCs.
        RpcError::UnknownMethod(_) => "This device's engine needs an update for that.".into(),
        other => other.to_string().into(),
    }
}

/// Whether `path` (relative to the context's root, not `""`) names an
/// entry there: its folder's listing has it. A listing cut short counts as
/// a yes (it may be past the cut), a failed one as a no.
pub(crate) async fn entry_exists(
    engine: &EngineHandle,
    context: &FilesContext,
    path: &str,
    cx: &mut gpui::AsyncApp,
) -> bool {
    let (parent, name) = match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    };
    let listing = call_with_retry(
        engine,
        methods::LIST_WORKSPACE_FILES,
        serde_json::Value::Object(context.params(parent)),
        cx,
    )
    .await
    .ok()
    .and_then(|value| serde_json::from_value::<WorkspaceDirectory>(value).ok());
    listing.is_some_and(|listing| {
        listing.truncated || listing.entries.iter().any(|entry| entry.name == name)
    })
}

/// One retry over a cold relay dial to the host device (the file mention
/// picker's exact courtesy), and a short wait when the host's file workers
/// are all busy.
pub(crate) async fn call_with_retry(
    engine: &EngineHandle,
    method: &str,
    params: serde_json::Value,
    cx: &mut gpui::AsyncApp,
) -> Result<serde_json::Value, RpcError> {
    let mut result = engine.client().call(method, params.clone()).await;
    for attempt in 0..3 {
        let retry = match &result {
            Err(RpcError::Transport(_) | RpcError::Closed) => attempt == 0,
            Err(err) => err.to_string().contains("busy; retry"),
            Ok(_) => false,
        };
        if !retry {
            break;
        }
        cx.background_executor()
            .timer(std::time::Duration::from_millis(250 * (attempt + 1)))
            .await;
        result = engine.client().call(method, params.clone()).await;
    }
    result
}

impl Render for FilesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.live.clone();
        live.scope(|| self.render_panel(window, cx))
    }
}

impl FilesPanel {
    /// The dock renders the panel cached, so animation ticks replay it:
    /// everything it shows comes from its own fields, and every change to
    /// them notifies. Loaders and hover fades inside paint through
    /// [`Self::live_layer`].
    fn render_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        for image in std::mem::take(&mut self.evicted) {
            gpui::ImageSource::Image(image).evict(Some(window), cx);
        }
        let theme = Theme::of(cx).clone();
        let measure = self.pane_width.clone();
        let wide = self.wide();
        let show_tree = if wide {
            self.tree_visible
        } else {
            self.tree_visible || self.selected.is_none()
        };
        let show_editor = wide || !show_tree;
        let tree = show_tree.then(|| {
            div()
                .flex_none()
                .h_full()
                .when(wide, |el| {
                    el.w(px(self.tree_column_width()))
                        .border_r_1()
                        .border_color(theme.border)
                })
                .when(!wide, |el| el.flex_1().min_w_0().pr(px(self.header_right)))
                .overflow_hidden()
                .child(self.render_tree_column(window, cx))
        });
        let editor = show_editor.then(|| {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .child(self.render_editor_side(window, cx))
        });
        let seam = (wide && show_tree).then(|| self.render_seam(&theme, cx));
        let menu = self.render_menu(&theme, cx);
        let confirm = self.render_confirm(&theme, window, cx);
        let panel = cx.entity_id();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .on_drag_move(cx.listener(
                move |this, event: &gpui::DragMoveEvent<TreeResize>, _, cx| {
                    if event.drag(cx).0 == panel {
                        let x = event.event.position.x - event.bounds.origin.x;
                        this.set_tree_width(f32::from(x), cx);
                    }
                },
            ))
            .child(
                gpui::canvas(
                    move |bounds, _, _| measure.set(f32::from(bounds.size.width)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .children(tree)
            .children(editor)
            .children(seam)
            .children(menu)
            .children(confirm)
            .into_any_element()
    }

    /// The panel's live layer: the dock paints its overlay right after the
    /// cached panel.
    pub fn live_layer(&self) -> LiveLayer {
        self.live.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_requires_a_project_session() {
        let mut state = AppState::new();
        assert!(context_for(&state).is_err());
        let mut chat = cypher_proto::Chat {
            id: "c1".into(),
            device_id: "dev-2".into(),
            cwd: Some("/work/repo".into()),
            created_at: chrono::Utc::now(),
            space_id: Some("s1".into()),
            ..crate::test_fixtures::chat()
        };
        state.chats.push(chat.clone());
        state.selected_chat = Some("c1".into());
        state.local_device_id = Some("dev-1".into());
        let context = context_for(&state).unwrap();
        assert_eq!(context.cwd, "/work/repo");
        assert_eq!(context.target.as_deref(), Some("dev-2"));
        let params = context.params("src/main.rs");
        assert_eq!(params["chatId"], "c1");
        assert_eq!(params["targetDeviceId"], "dev-2");
        assert_eq!(params["path"], "src/main.rs");

        state.local_device_id = Some("dev-2".into());
        assert_eq!(context_for(&state).unwrap().target, None);

        chat.space_id = None;
        state.chats[0] = chat.clone();
        assert!(context_for(&state).is_err());
        chat.space_id = Some("s1".into());
        chat.cwd = None;
        state.chats[0] = chat;
        assert!(context_for(&state).is_err());
    }

    /// A workspace's own chat, in its folder, granted "api" (a checkout)
    /// and "web" (plain folder), as the engine lists them.
    fn workspace_state() -> AppState {
        let mut state = AppState::new();
        state.bots = vec![crate::test_fixtures::bot()];
        state.chats.push(cypher_proto::Chat {
            id: "bot-chat".into(),
            device_id: "dev-2".into(),
            cwd: Some("/data/bots/b".into()),
            bot: Some(cypher_proto::BotChat::owner("b")),
            ..crate::test_fixtures::chat()
        });
        state.selected_chat = Some("bot-chat".into());
        state.local_device_id = Some("dev-1".into());
        state
    }

    fn root(space: Option<&str>, name: &str, path: &str) -> cypher_proto::ChatFileRoot {
        cypher_proto::ChatFileRoot {
            space_id: space.map(str::to_owned),
            name: name.into(),
            path: path.into(),
            checkout_id: (space == Some("api")).then(|| "co-api".to_string()),
        }
    }

    #[test]
    fn a_workspace_chat_browses_its_picked_root() {
        let mut state = workspace_state();
        // Its roots on their way: no context yet (not the session refusal).
        assert_eq!(context_for(&state), Err("Loading…"));
        state.set_file_roots_for_test(vec![
            root(None, "Bot", "/private/data/bots/b"),
            root(Some("api"), "api", "/src/api"),
            root(Some("web"), "web", "/src/web"),
        ]);
        // The first granted project by default; every request carries the
        // chat's own cwd and names the project.
        let context = context_for(&state).unwrap();
        assert_eq!(context.root_space_id.as_deref(), Some("api"));
        assert_eq!(context.root, "/src/api");
        let params = context.params("src/lib.rs");
        assert_eq!(params["cwd"], "/data/bots/b");
        assert_eq!(params["rootSpaceId"], "api");
        assert_eq!(params["targetDeviceId"], "dev-2");
        // The workspace's own folder: no rootSpaceId.
        state.chat_roots_pick_for_test(None);
        let context = context_for(&state).unwrap();
        assert_eq!(context.root, "/private/data/bots/b");
        assert!(!context.params("notes.md").contains_key("rootSpaceId"));
        // Git never shows the plain folder: the pick's checkout, else the
        // first one.
        assert_eq!(state.git_root().unwrap().name, "api");
        state.chat_roots_pick_for_test(Some("web"));
        assert_eq!(context_for(&state).unwrap().root, "/src/web");
        assert_eq!(state.git_root().unwrap().name, "api");
        let git = state.git_chat_row().unwrap();
        assert_eq!(git.cwd.as_deref(), Some("/src/api"));
        assert_eq!(git.checkout_id.as_deref(), Some("co-api"));
        assert_eq!(git.space_id.as_deref(), Some("api"));
        // References look in every root, the chat's folder first.
        let links = link_contexts(&state).unwrap();
        assert!(links.multi_root());
        let roots: Vec<_> = links
            .contexts
            .iter()
            .map(|c| c.root_space_id.as_deref())
            .collect();
        assert_eq!(roots, vec![None, Some("api"), Some("web")]);
    }

    #[test]
    fn an_older_engine_shows_the_workspace_folder_without_a_picker() {
        let mut state = workspace_state();
        let fallback =
            crate::files::roots::fallback_roots(state.selected_chat_row().unwrap(), "Bot");
        state.set_file_roots_for_test(fallback);
        let context = context_for(&state).unwrap();
        assert_eq!(context.root_space_id, None);
        assert_eq!(context.root, "/data/bots/b");
        // One root: the picker has nothing to offer; no checkout for Git.
        assert!(
            matches!(state.file_roots(), crate::state::FileRoots::Ready(list) if list.len() == 1)
        );
        assert!(state.git_root().is_none());
        assert!(state.git_chat_row().is_none());
    }

    #[test]
    fn a_session_keeps_its_single_checkout() {
        let mut state = AppState::new();
        state.chats.push(cypher_proto::Chat {
            id: "c1".into(),
            cwd: Some("/work/repo".into()),
            space_id: Some("s1".into()),
            ..crate::test_fixtures::chat()
        });
        state.selected_chat = Some("c1".into());
        assert_eq!(state.file_roots(), crate::state::FileRoots::None);
        let links = link_contexts(&state).unwrap();
        assert!(!links.multi_root());
        assert_eq!(links.contexts[0].root, "/work/repo");
        assert!(!links.contexts[0].params("a").contains_key("rootSpaceId"));
        assert!(matches!(
            state.git_chat_row(),
            Some(std::borrow::Cow::Borrowed(_))
        ));
    }

    #[test]
    fn markdown_files_are_the_ones_that_preview() {
        assert!(is_markdown("README.md"));
        assert!(is_markdown("docs/guide.markdown"));
        assert!(!is_markdown("src/main.rs"));
        assert!(!is_markdown("notes.txt"));
    }

    #[test]
    fn every_supported_language_round_trips_through_its_fence_tag() {
        for lang in [
            LanguageId::Rust,
            LanguageId::TypeScript,
            LanguageId::Python,
            LanguageId::Markdown,
            LanguageId::Dockerfile,
        ] {
            assert_eq!(
                cypher_syntax::language_for_alias(fence_tag(lang)),
                Some(lang)
            );
        }
    }

    #[test]
    fn the_tree_column_leaves_room_for_the_file() {
        assert_eq!(clamp_tree_width(300.0, 1000.0), 300.0);
        assert_eq!(clamp_tree_width(900.0, 1000.0), 1000.0 - EDITOR_MIN_WIDTH);
        assert_eq!(clamp_tree_width(40.0, 1000.0), TREE_MIN_WIDTH);
        // The narrowest split pane still shows a usable tree.
        assert_eq!(clamp_tree_width(TREE_WIDTH, SPLIT_MIN_WIDTH), TREE_WIDTH);
        const { assert!(SPLIT_MIN_WIDTH - EDITOR_MIN_WIDTH >= TREE_MIN_WIDTH) };
    }

    #[test]
    fn human_bytes_reads_naturally() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }
}
