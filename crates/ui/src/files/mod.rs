//! The right pane's Files surface: a lazy directory tree over the selected
//! chat's checkout (the engine's read-only `ListWorkspaceFiles` browser —
//! built for iOS, now shared) beside a syntax-colored [`editor::CodeEditor`]
//! for the picked file, saving back through `WriteWorkspaceFile`.
//!
//! Everything is fetched from the chat's HOST device (`targetDeviceId` relay
//! forwarding, exactly like the diff pane): the tree is that checkout's, not
//! this machine's. Directories load on first expand; a file loads on first
//! open and keeps its editor (and any unsaved text) for the surface's life.
//!
//! Layout adapts to the pane: wide panes show tree + editor side by side;
//! narrow ones show one at a time, toggled from the header.
//!
//! Markdown files open in a rendered PREVIEW (the transcript's own Markdown
//! renderer, fenced code highlighted in the background); a header toggle
//! flips to the source editor and back. The preview re-parses lazily — only
//! when it is shown and the text changed since the last parse.

pub mod editor;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, IntoElement, Render, SharedString,
    Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};

use cypher_proto::{WorkspaceDirectory, WorkspaceFileContent, WorkspaceFileEntry};
use cypher_rpc::{RpcError, methods};
use cypher_syntax::{HighlightedDocument, LanguageId};

use crate::icons::{self, icon};
use crate::markdown::parser::{Block, BlockTree, parse_full};
use crate::markdown::render;
use crate::state::AppState;
use crate::theme::Theme;

use editor::{CodeEditor, CodeEditorEvent};

/// Tree column width when the pane is wide enough to split.
pub const TREE_WIDTH: f32 = 224.0;
/// Below this pane width the tree and the editor take turns.
pub const SPLIT_MIN_WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 24.0;
const INDENT: f32 = 14.0;
/// Breathing room between the tree column's edges and a row's hover/selected
/// pill. The list carries it as padding, so rows still fill the width inside.
const ROW_INSET: f32 = 6.0;
const ROW_RADIUS: f32 = 6.0;

/// The request context a Files surface is bound to — the selected chat's
/// checkout on its host device. Re-read per request (like the diff pane)
/// so the engine's own "chat device or checkout changed" guard is the only
/// thing that can go stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesContext {
    pub chat_id: String,
    pub cwd: String,
    /// The host device when it is not the connected engine's own.
    pub target: Option<String>,
}

impl FilesContext {
    fn params(&self, path: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), self.chat_id.clone().into());
        params.insert("cwd".into(), self.cwd.clone().into());
        params.insert("path".into(), path.to_string().into());
        if let Some(target) = &self.target {
            params.insert("targetDeviceId".into(), target.clone().into());
        }
        params
    }
}

/// Resolve the Files context from app state. Pure over the state's rows.
pub fn context_for(state: &AppState) -> Result<FilesContext, &'static str> {
    let chat = state
        .selected_chat_row()
        .ok_or("Open a session to browse its files.")?;
    let cwd = chat
        .cwd
        .clone()
        .ok_or("This session has no project folder.")?;
    if chat.space_id.is_none() {
        return Err("This session is not attached to a project.");
    }
    let target = (state.local_device_id.as_deref() != Some(chat.device_id.as_str()))
        .then(|| chat.device_id.clone());
    Ok(FilesContext {
        chat_id: chat.id.clone(),
        cwd,
        target,
    })
}

#[derive(Debug, Default)]
struct DirState {
    entries: Vec<WorkspaceFileEntry>,
    truncated: bool,
    loading: bool,
    loaded: bool,
    error: Option<SharedString>,
}

/// One flattened tree row (the visible, depth-first order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub loading: bool,
    /// Directory placeholder rows: "Loading…", "Empty", errors, "…more".
    pub note: Option<SharedString>,
}

/// Flatten the loaded directories into rows, depth-first, following
/// `expanded`. Pure so the tree order is testable without a shell.
fn flatten_tree(dirs: &HashMap<String, DirState>, expanded: &HashSet<String>) -> Vec<TreeRow> {
    fn walk(
        dir_path: &str,
        depth: usize,
        dirs: &HashMap<String, DirState>,
        expanded: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        let Some(dir) = dirs.get(dir_path) else {
            return;
        };
        let note = |text: &str| TreeRow {
            path: format!("{dir_path}\u{0}note"),
            name: String::new(),
            is_dir: false,
            depth,
            expanded: false,
            loading: false,
            note: Some(text.to_string().into()),
        };
        if let Some(error) = &dir.error {
            out.push(note(error));
            return;
        }
        if dir.loading && !dir.loaded {
            out.push(note("Loading…"));
            return;
        }
        if dir.loaded && dir.entries.is_empty() {
            out.push(note("Empty folder"));
            return;
        }
        for entry in &dir.entries {
            let path = if dir_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{dir_path}/{}", entry.name)
            };
            let is_expanded = entry.is_dir && expanded.contains(&path);
            out.push(TreeRow {
                path: path.clone(),
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                depth,
                expanded: is_expanded,
                loading: entry.is_dir && dirs.get(&path).is_some_and(|d| d.loading && !d.loaded),
                note: None,
            });
            if is_expanded {
                walk(&path, depth + 1, dirs, expanded, out);
            }
        }
        if dir.truncated {
            out.push(note("… listing cut at 1000 entries"));
        }
    }
    let mut rows = Vec::new();
    walk("", 0, dirs, expanded, &mut rows);
    rows
}

struct OpenFile {
    editor: Entity<CodeEditor>,
    /// The text as last loaded or saved — the dirty baseline.
    clean: Option<String>,
    dirty: bool,
    loading: bool,
    saving: bool,
    binary: bool,
    truncated: bool,
    bytes: u64,
    error: Option<SharedString>,
    /// Markdown only: show the rendered preview instead of the source.
    preview: bool,
    /// The last parsed preview; `preview_stale` marks it behind the text.
    preview_tree: Option<Arc<BlockTree>>,
    preview_stale: bool,
    /// Fenced-code highlights by top-level block index, for `preview_tree`.
    preview_highlights: HashMap<usize, Arc<HighlightedDocument>>,
    preview_gen: u64,
    _preview_task: Option<Task<()>>,
    _sub: Subscription,
}

/// Whether `path` opens in the Markdown preview (by extension/filename,
/// the syntax crate's registry).
fn is_markdown(path: &str) -> bool {
    cypher_syntax::language_for_path(path) == Some(LanguageId::Markdown)
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
    /// Bumped whenever the checkout context changes so stale replies drop.
    generation: u64,
    context: Option<FilesContext>,
    context_error: Option<SharedString>,
    /// Keeps directory fetches alive; keyed so a re-request replaces the
    /// previous one for the same folder.
    dir_tasks: HashMap<String, Task<()>>,
    file_tasks: HashMap<String, Task<()>>,
    _observe: Subscription,
}

impl FilesPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
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
            generation: 0,
            context: None,
            context_error: None,
            dir_tasks: HashMap::new(),
            file_tasks: HashMap::new(),
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

    /// True when any open file has unsaved edits (the shell asks before
    /// closing the tab).
    pub fn has_unsaved(&self) -> bool {
        self.open_files.values().any(|f| f.dirty)
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
        self.open_files.clear();
        self.dir_tasks.clear();
        self.file_tasks.clear();
    }

    fn rebuild_rows(&mut self) {
        self.rows = flatten_tree(&self.dirs, &self.expanded);
    }

    fn load_dir(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(context) = self.context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
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

    /// Show `path` in the editor side, loading it on first open. Narrow
    /// panes flip to the editor; `window` focuses it for typing.
    pub fn open_file(&mut self, path: String, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.selected = Some(path.clone());
        if self.pane_width.get() < SPLIT_MIN_WIDTH {
            self.tree_visible = false;
        }
        if !self.open_files.contains_key(&path) {
            let editor = cx.new(|cx| CodeEditor::new(&path, cx));
            let sub_path = path.clone();
            let sub = cx.subscribe(
                &editor,
                move |this: &mut Self, editor, event, cx| match event {
                    CodeEditorEvent::Edited => {
                        if let Some(file) = this.open_files.get_mut(&sub_path) {
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
                    CodeEditorEvent::Save => this.save_file(sub_path.clone(), cx),
                },
            );
            self.open_files.insert(
                path.clone(),
                OpenFile {
                    editor,
                    clean: None,
                    dirty: false,
                    loading: false,
                    saving: false,
                    binary: false,
                    truncated: false,
                    bytes: 0,
                    error: None,
                    preview: is_markdown(&path),
                    preview_tree: None,
                    preview_stale: true,
                    preview_highlights: HashMap::new(),
                    preview_gen: 0,
                    _preview_task: None,
                    _sub: sub,
                },
            );
            self.load_file(path.clone(), cx);
        }
        if let (Some(window), Some(file)) = (window, self.open_files.get(&path)) {
            let handle = file.editor.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn load_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(context) = self.context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
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
                        // A truncated read must never be saved back — it
                        // would silently drop the file's tail.
                        let read_only = content.binary || content.truncated;
                        file.clean = Some(text.clone());
                        file.dirty = false;
                        file.preview_stale = true;
                        file.editor
                            .update(cx, |editor, cx| editor.set_content(text, read_only, cx));
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

    fn save_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(context) = self.context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
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
            self.load_file(path, cx);
        }
    }

    fn toggle_tree(&mut self, cx: &mut Context<Self>) {
        self.tree_visible = !self.tree_visible;
        cx.notify();
    }

    /// The header's Source/Preview toggle (Markdown files). Switching to
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

    /// Re-parse the preview when it is shown and the text moved on; kicks a
    /// background highlight of its fenced code (paint-only when it lands).
    fn ensure_preview(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(file) = self.open_files.get_mut(path) else {
            return;
        };
        if !file.preview || !file.preview_stale || file.clean.is_none() {
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
                Block::CodeBlock {
                    language: Some(tag),
                    code,
                } => cypher_syntax::language_for_alias(tag)
                    .filter(|lang| cypher_syntax::supports_language(*lang))
                    .map(|lang| (ix, lang, code.clone())),
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

    // ---- rendering ----

    /// The surface's second header row (the shell hosts it above the body):
    /// tree toggle · path · save state · refresh.
    pub fn render_header_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let selected = self.selected.clone();
        let file = selected.as_deref().and_then(|p| self.open_files.get(p));
        let dirty = file.is_some_and(|f| f.dirty);
        let saving = file.is_some_and(|f| f.saving);
        let read_only = file.is_some_and(|f| f.editor.read(cx).read_only());
        let position = file.map(|f| f.editor.read(cx).caret_position());
        let error = file.and_then(|f| f.error.clone());
        let preview = file.map(|f| f.preview);
        let markdown = selected.as_deref().is_some_and(is_markdown) && file.is_some();
        let tree_on = self.tree_visible;
        let wide = self.wide();
        let path_label: SharedString = match &selected {
            Some(path) => path.clone().into(),
            None => "Files".into(),
        };

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
        if markdown {
            let preview_on = preview.unwrap_or(false);
            let chip = |id: &'static str, label: &'static str, active: bool| {
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
                        el.bg(crate::theme::wash(0.12)).text_color(theme.text)
                    })
                    .when(!active, |el| {
                        el.text_color(theme.text_muted)
                            .hover(|s| s.bg(crate::theme::wash(0.06)))
                    })
                    .child(SharedString::from(label))
            };
            row = row.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.0))
                    .p(px(1.0))
                    .rounded(px(6.0))
                    .bg(crate::theme::ink(0.04))
                    .child(chip("files-md-source", "Source", !preview_on).on_click(
                        cx.listener(|this, _, window, cx| this.set_preview(false, window, cx)),
                    ))
                    .child(chip("files-md-preview", "Preview", preview_on).on_click(
                        cx.listener(|this, _, window, cx| this.set_preview(true, window, cx)),
                    )),
            );
        }
        if let Some((line, column)) = position
            && selected.is_some()
            && preview != Some(true)
        {
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
                    .bg(crate::theme::ink(0.05))
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

    fn render_tree(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if let Some(error) = &self.context_error {
            return centered_note(error.clone(), &theme);
        }
        let count = self.rows.len();
        let selected = self.selected.clone();
        let dirty: HashSet<String> = self
            .open_files
            .iter()
            .filter(|(_, f)| f.dirty)
            .map(|(p, _)| p.clone())
            .collect();
        let rows = self.rows.clone();
        let entity = cx.entity();
        uniform_list("files-tree", count, move |range, _window, cx| {
            let mut out = Vec::with_capacity(range.len());
            for ix in range {
                let Some(row) = rows.get(ix) else {
                    continue;
                };
                let is_selected = selected.as_deref() == Some(row.path.as_str());
                let is_dirty = dirty.contains(&row.path);
                out.push(render_tree_row(
                    ix,
                    row,
                    is_selected,
                    is_dirty,
                    &theme,
                    entity.clone(),
                    cx,
                ));
            }
            out
        })
        .track_scroll(&self.tree_scroll)
        .size_full()
        .py(px(4.0))
        .px(px(ROW_INSET))
        .into_any_element()
    }

    fn render_editor_side(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(path) = self.selected.clone() else {
            return centered_note("Select a file to view it.".into(), &theme);
        };
        self.ensure_preview(&path, cx);
        let Some(file) = self.open_files.get(&path) else {
            return centered_note("Select a file to view it.".into(), &theme);
        };
        if file.loading && file.clean.is_none() {
            return centered_note("Loading…".into(), &theme);
        }
        if let Some(error) = &file.error
            && file.clean.is_none()
        {
            return centered_note(error.clone(), &theme);
        }
        if file.binary {
            return centered_note(
                format!("Binary or non-UTF-8 file ({}).", human_bytes(file.bytes)).into(),
                &theme,
            );
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
        let body: AnyElement = match (file.preview, &file.preview_tree) {
            (true, Some(tree)) => {
                let options = render::RenderOptions::settled(format!("files-md:{path}").into());
                let highlights = file.preview_highlights.clone();
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
                    .child(div().w_full().max_w(px(760.0)).child(rendered))
                    .into_any_element()
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
}

/// The fence alias `cypher_syntax` resolves back to `lang` (the transcript's
/// table): highlighting a fenced block goes through the alias path so the
/// parser picks the same injections it would for a chat reply.
fn fence_tag(lang: LanguageId) -> &'static str {
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

fn render_tree_row(
    ix: usize,
    row: &TreeRow,
    selected: bool,
    dirty: bool,
    theme: &Theme,
    panel: Entity<FilesPanel>,
    _cx: &mut App,
) -> AnyElement {
    let indent = 4.0 + row.depth as f32 * INDENT;
    if let Some(note) = &row.note {
        return div()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(indent + 18.0))
            .pr(px(8.0))
            .flex()
            .items_center()
            .text_size(px(11.0))
            .text_color(theme.text_faint)
            .italic()
            .child(div().truncate().child(note.clone()))
            .into_any_element();
    }
    let path = row.path.clone();
    let is_dir = row.is_dir;
    let chevron = if row.is_dir {
        Some(if row.expanded {
            icons::ALT_ARROW_DOWN
        } else {
            icons::ALT_ARROW_RIGHT
        })
    } else {
        None
    };
    let glyph = if row.is_dir {
        icons::FOLDER
    } else {
        icons::DOCUMENT
    };
    div()
        .id(("files-row", ix))
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(indent))
        .pr(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .cursor_pointer()
        .rounded(px(ROW_RADIUS))
        .when(selected, |el| el.bg(crate::theme::wash(0.10)))
        .when(!selected, |el| el.hover(|s| s.bg(crate::theme::wash(0.05))))
        .on_click(move |_, window, cx| {
            panel.update(cx, |panel, cx| {
                if is_dir {
                    panel.toggle_dir(path.clone(), cx);
                } else {
                    panel.open_file(path.clone(), Some(window), cx);
                }
            });
        })
        .child(
            div()
                .flex_none()
                .size(px(12.0))
                .flex()
                .items_center()
                .justify_center()
                .when_some(chevron, |el, chevron| {
                    el.child(icon(chevron).size(px(10.0)).text_color(theme.text_faint))
                }),
        )
        .child(
            icon(glyph)
                .size(px(12.0))
                .flex_none()
                .text_color(if row.is_dir {
                    theme.text_muted
                } else {
                    theme.text_faint
                }),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(12.0))
                .text_color(if selected {
                    theme.text
                } else {
                    theme.text.opacity(0.85)
                })
                .child(SharedString::from(row.name.clone())),
        )
        .when(dirty, |el| {
            el.child(
                div()
                    .flex_none()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme.accent),
            )
        })
        .when(row.loading, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from("…")),
            )
        })
        .into_any_element()
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
        .when(active, |el| el.bg(crate::theme::wash(0.10)))
        .hover(|s| s.bg(crate::theme::wash(0.12)))
        .child(icon(icon_path).size(px(13.0)).text_color(theme.text_muted))
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// User-facing text for a failed workspace RPC.
fn rpc_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::Transport(_) | RpcError::Closed => "Could not reach the session's device.".into(),
        // Old engines have no WriteWorkspaceFile at all.
        RpcError::UnknownMethod(_) => "This device's engine needs an update for that.".into(),
        other => other.to_string().into(),
    }
}

/// One retry over a cold relay dial to the host device (the file mention
/// picker's exact courtesy).
async fn call_with_retry(
    engine: &crate::state::EngineHandle,
    method: &str,
    params: serde_json::Value,
    cx: &mut gpui::AsyncApp,
) -> Result<serde_json::Value, RpcError> {
    let mut result = engine.client().call(method, params.clone()).await;
    if matches!(result, Err(RpcError::Transport(_)) | Err(RpcError::Closed)) {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(250))
            .await;
        result = engine.client().call(method, params).await;
    }
    result
}

impl Render for FilesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    el.w(px(TREE_WIDTH)).border_r_1().border_color(theme.border)
                })
                .when(!wide, |el| el.flex_1().min_w_0())
                .overflow_hidden()
                .child(self.render_tree(cx))
        });
        let editor = show_editor.then(|| {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .child(self.render_editor_side(window, cx))
        });
        div()
            .size_full()
            .relative()
            .flex()
            .flex_row()
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(entries: &[(&str, bool)], loaded: bool) -> DirState {
        DirState {
            entries: entries
                .iter()
                .map(|(name, is_dir)| WorkspaceFileEntry {
                    name: name.to_string(),
                    is_dir: *is_dir,
                })
                .collect(),
            truncated: false,
            loading: false,
            loaded,
            error: None,
        }
    }

    #[test]
    fn flatten_follows_expansion_depth_first() {
        let mut dirs = HashMap::new();
        dirs.insert(
            String::new(),
            dir(&[("src", true), ("Cargo.toml", false)], true),
        );
        dirs.insert("src".into(), dir(&[("lib.rs", false)], true));
        let mut expanded = HashSet::new();
        let rows = flatten_tree(&dirs, &expanded);
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["src", "Cargo.toml"]
        );
        expanded.insert("src".into());
        let rows = flatten_tree(&dirs, &expanded);
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["src", "src/lib.rs", "Cargo.toml"]
        );
        assert_eq!(rows[1].depth, 1);
        assert!(rows[0].expanded);
    }

    #[test]
    fn placeholder_rows_explain_loading_empty_and_errors() {
        let mut dirs = HashMap::new();
        dirs.insert(String::new(), dir(&[("a", true), ("b", true)], true));
        dirs.insert(
            "a".into(),
            DirState {
                loading: true,
                ..Default::default()
            },
        );
        dirs.insert("b".into(), dir(&[], true));
        let expanded: HashSet<String> = ["a".to_string(), "b".to_string()].into();
        let rows = flatten_tree(&dirs, &expanded);
        assert_eq!(rows[1].note.as_deref(), Some("Loading…"));
        assert!(rows[0].loading);
        assert_eq!(rows[3].note.as_deref(), Some("Empty folder"));
        let mut truncated = dir(&[("x", false)], true);
        truncated.truncated = true;
        dirs.insert(String::new(), truncated);
        let rows = flatten_tree(&dirs, &HashSet::new());
        assert!(
            rows.last()
                .unwrap()
                .note
                .as_deref()
                .unwrap()
                .contains("1000")
        );
    }

    #[test]
    fn context_requires_a_project_session() {
        let mut state = AppState::new();
        assert!(context_for(&state).is_err());
        let mut chat = cypher_proto::Chat {
            pinned: false,
            id: "c1".into(),
            device_id: "dev-2".into(),
            title: None,
            archived: false,
            cwd: Some("/work/repo".into()),
            branch: None,
            checkout_id: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: Some("s1".into()),
            last_seen_at: None,
            room_gen: None,
            child: None,
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
    fn human_bytes_reads_naturally() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}
