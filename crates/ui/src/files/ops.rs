//! The file manager's verbs: new file/folder and rename through the inline
//! name field, cut/copy/paste/duplicate and drag moves, delete (Trash first),
//! importing dropped or pasted Finder files, and the path actions (copy path,
//! reveal, open with the default app, mention in chat). Every mutation runs on
//! the chat's host device through the jailed workspace-entry RPCs.

use std::collections::HashSet;
use std::path::PathBuf;

use base64::Engine as _;
use gpui::{ClipboardEntry, ClipboardItem, Context, SharedString, Window, prelude::*};

use cypher_rpc::{RpcError, methods};

use super::tree::{EditTarget, InlineEdit, join, name_of, name_problem, parent_of, stem_range};
use super::{FilesEvent, FilesPanel, rpc_message};
use crate::widgets::text_input::{TextInput, TextInputEvent};

/// Rows cut or copied for a later paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileClipboard {
    pub items: Vec<(String, bool)>,
    pub cut: bool,
    /// The text this copy also put on the system clipboard — a different
    /// system clipboard means something newer was copied since.
    pub marker: String,
}

/// Raw bytes per import slice (base64 rides under the relay's frame cap).
const IMPORT_CHUNK: usize = 384 * 1024;
const IMPORT_ENTRY_LIMIT: usize = 10_000;
const IMPORT_BYTE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// Finder's duplicate names: "a copy.txt", "a copy 2.txt".
pub(super) fn copy_name(name: &str, is_dir: bool, n: usize) -> String {
    let (stem, ext) = split_ext(name, is_dir);
    if n <= 1 {
        format!("{stem} copy{ext}")
    } else {
        format!("{stem} copy {n}{ext}")
    }
}

/// Finder's "keep both" names for imports: "a 2.txt", "a 3.txt".
pub(super) fn numbered_name(name: &str, is_dir: bool, n: usize) -> String {
    if n <= 1 {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name, is_dir);
    format!("{stem} {n}{ext}")
}

fn split_ext(name: &str, is_dir: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

/// The first `make(n)` (n = 1, 2, …) not in `taken` (lowercased names —
/// macOS volumes are case-insensitive).
pub(super) fn free_name(
    taken: &HashSet<String>,
    make: impl Fn(usize) -> String,
) -> (String, usize) {
    for n in 1..10_000 {
        let candidate = make(n);
        if !taken.contains(&candidate.to_lowercase()) {
            return (candidate, n);
        }
    }
    (make(10_000), 10_000)
}

fn is_conflict(err: &RpcError) -> bool {
    err.to_string().contains("already exists")
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// One local item to import: its path relative to the dropped item's new
/// name, and its source (`None` for a folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImportItem {
    pub rel: String,
    pub source: Option<(PathBuf, u64)>,
}

/// One dropped top-level item and everything under it, folders first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImportPlan {
    pub name: String,
    pub is_dir: bool,
    pub items: Vec<ImportItem>,
}

/// Walk dropped local paths (never following links; git metadata and
/// special files skipped) within the import limits.
pub(super) fn plan_import(paths: &[PathBuf]) -> Result<(Vec<ImportPlan>, u64), String> {
    fn walk(
        dir: &std::path::Path,
        rel: &str,
        items: &mut Vec<ImportItem>,
        totals: &mut (usize, u64),
    ) -> Result<(), String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.eq_ignore_ascii_case(".git") || name_problem(&name, false).is_some() {
                continue;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            let child = join(rel, &name);
            totals.0 += 1;
            if totals.0 > IMPORT_ENTRY_LIMIT {
                return Err("Too many files to import at once.".into());
            }
            if meta.is_dir() {
                items.push(ImportItem {
                    rel: child.clone(),
                    source: None,
                });
                walk(&entry.path(), &child, items, totals)?;
            } else if meta.is_file() {
                totals.1 += meta.len();
                if totals.1 > IMPORT_BYTE_LIMIT {
                    return Err("Too much data to import at once.".into());
                }
                items.push(ImportItem {
                    rel: child,
                    source: Some((entry.path(), meta.len())),
                });
            }
        }
        Ok(())
    }
    let mut plans = Vec::new();
    let mut totals = (0usize, 0u64);
    for path in paths {
        let meta = path
            .symlink_metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if name_problem(&name, false).is_some() {
            continue;
        }
        totals.0 += 1;
        if meta.is_dir() {
            let mut items = Vec::new();
            walk(path, "", &mut items, &mut totals)?;
            plans.push(ImportPlan {
                name,
                is_dir: true,
                items,
            });
        } else if meta.is_file() {
            totals.1 += meta.len();
            if totals.1 > IMPORT_BYTE_LIMIT {
                return Err("Too much data to import at once.".into());
            }
            plans.push(ImportPlan {
                name,
                is_dir: false,
                items: vec![ImportItem {
                    rel: String::new(),
                    source: Some((path.clone(), meta.len())),
                }],
            });
        }
    }
    Ok((plans, totals.1))
}

impl FilesPanel {
    // ---- inline name field ----

    /// Start a new file/folder in `parent`: the folder expands and an empty
    /// name field leads its rows.
    pub(super) fn begin_create(
        &mut self,
        parent: String,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context.is_none() {
            return;
        }
        self.clear_filter(cx);
        if !parent.is_empty() {
            self.reveal_in_tree(parent.clone(), cx);
            self.expanded.insert(parent.clone());
            if !self
                .dirs
                .get(&parent)
                .is_some_and(|d| d.loaded || d.loading)
            {
                self.load_dir(parent.clone(), cx);
            }
        }
        let placeholder = if directory {
            "Folder name"
        } else {
            "File name"
        };
        self.start_edit(
            EditTarget::Create { parent, directory },
            placeholder,
            String::new(),
            None,
            window,
            cx,
        );
    }

    /// Rename `path` in place, the stem preselected.
    pub(super) fn begin_rename(
        &mut self,
        path: String,
        is_dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name_of(&path).to_string();
        let range = stem_range(&name, is_dir);
        self.start_edit(
            EditTarget::Rename { path, is_dir },
            "Name",
            name,
            Some(range),
            window,
            cx,
        );
    }

    fn start_edit(
        &mut self,
        target: EditTarget,
        placeholder: &'static str,
        text: String,
        select: Option<std::ops::Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            let mut input = TextInput::settings_field(placeholder, false, cx);
            input.set_metrics(12.0, 16.0);
            input.set_text(text, cx);
            if let Some(range) = select {
                input.select_range(range, cx);
            }
            input
        });
        let events =
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event, window, cx| match event {
                    TextInputEvent::Submitted => this.commit_edit(window, cx),
                    TextInputEvent::Edited => {
                        if let Some(edit) = this.edit.as_mut()
                            && edit.error.take().is_some()
                        {
                            cx.notify();
                        }
                    }
                    _ => {}
                },
            );
        // Clicking away commits, like Finder.
        let blur = cx.on_blur(
            &input.read(cx).focus_handle.clone(),
            window,
            |this: &mut Self, window, cx| {
                if this.edit.is_some() {
                    this.commit_edit(window, cx);
                }
            },
        );
        self.edit = Some(InlineEdit {
            target,
            input,
            error: None,
            focus_pending: true,
            _subs: vec![events, blur],
        });
        self.rebuild_rows();
        self.scroll_to_edit();
        cx.notify();
    }

    pub(super) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.take().is_some() {
            self.rebuild_rows();
            window.focus(&self.tree_focus, cx);
            cx.notify();
        }
    }

    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let raw = edit.input.read(cx).text().trim().to_string();
        let target = edit.target.clone();
        match &target {
            EditTarget::Rename { path, .. } if raw == name_of(path) => {
                self.cancel_edit(window, cx);
                return;
            }
            EditTarget::Create { .. } if raw.is_empty() => {
                self.cancel_edit(window, cx);
                return;
            }
            _ => {}
        }
        let nested = matches!(target, EditTarget::Create { .. });
        if let Some(problem) = name_problem(&raw, nested) {
            edit.error = Some(problem.into());
            cx.notify();
            return;
        }
        self.edit = None;
        self.rebuild_rows();
        window.focus(&self.tree_focus, cx);
        match target {
            EditTarget::Rename { path, is_dir } => {
                let to = join(parent_of(&path), &raw);
                self.run_moves(vec![(path, is_dir, to)], cx);
            }
            EditTarget::Create { parent, directory } => self.run_create(parent, raw, directory, cx),
        }
        cx.notify();
    }

    /// Create `name` (which may be `a/b/c`) inside `parent`; missing folders
    /// on the way are made too. A new file opens.
    fn run_create(
        &mut self,
        parent: String,
        name: String,
        directory: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let parts: Vec<String> = name.split('/').map(str::to_string).collect();
        let task = cx.spawn(async move |this, cx| {
            let mut at = parent.clone();
            let mut result = Ok(());
            for (i, part) in parts.iter().enumerate() {
                at = join(&at, part);
                let last = i + 1 == parts.len();
                let mut params = context.params(&at);
                params.insert("directory".into(), (!last || directory).into());
                match engine
                    .client()
                    .call(
                        methods::CREATE_WORKSPACE_ENTRY,
                        serde_json::Value::Object(params),
                    )
                    .await
                {
                    Ok(_) => {}
                    // An intermediate folder that already exists is fine.
                    Err(err) if !last && is_conflict(&err) => {}
                    Err(err) => {
                        result = Err(err);
                        break;
                    }
                }
            }
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if result.is_ok() {
                    for ancestor in ancestors(&at) {
                        if ancestor != at || directory {
                            this.expanded.insert(ancestor);
                        }
                    }
                }
                this.reload_dirs([parent.clone(), parent_of(&at).to_string()], cx);
                match result {
                    Ok(()) => {
                        this.selection.single(&at);
                        this.reveal_pending = Some(at.clone());
                        if !directory {
                            // A new file is for writing: source, not preview.
                            this.open_file(at.clone(), true, None, cx);
                            if let Some(file) = this.open_files.get_mut(&at) {
                                file.preview = false;
                            }
                        }
                        this.set_status(None);
                    }
                    Err(err) => this.set_error(rpc_message(&err)),
                }
                this.rebuild_rows();
                cx.notify();
            })
            .ok();
        });
        self.keep_task(task);
    }

    // ---- move / copy / paste ----

    /// Cut or copy the selection for a later paste.
    pub(super) fn copy_entries(
        &mut self,
        items: Vec<(String, bool)>,
        cut: bool,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let marker = items
            .iter()
            .map(|(path, _)| self.absolute_path(path))
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(marker.clone()));
        let count = items.len();
        self.clipboard = Some(super::ops::FileClipboard { items, cut, marker });
        self.set_status(Some(format!(
            "{} {}",
            if cut { "Cut" } else { "Copied" },
            plural(count, "item")
        )));
        cx.notify();
    }

    /// Whether ⌘V has something to paste here: our own clipboard, or files
    /// copied in Finder.
    pub(super) fn can_paste(&self, cx: &gpui::App) -> bool {
        self.clipboard.is_some() || !external_clipboard_paths(cx).is_empty()
    }

    /// Paste into `dir`: Finder files copied since our own copy win (they
    /// import); otherwise our clipboard moves (cut) or copies.
    pub(super) fn paste(&mut self, dir: String, cx: &mut Context<Self>) {
        let external = external_clipboard_paths(cx);
        let ours_current = self.clipboard.as_ref().is_some_and(|clip| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .is_some_and(|text| text == clip.marker)
        });
        if !external.is_empty() && !ours_current {
            self.import(external, dir, cx);
            return;
        }
        let Some(clip) = self.clipboard.clone() else {
            return;
        };
        if clip.cut {
            self.clipboard = None;
        }
        self.transfer(clip.items, dir, !clip.cut, cx);
    }

    /// Move (or copy) entries into `dir` — paste and drag-and-drop.
    pub(super) fn transfer(
        &mut self,
        items: Vec<(String, bool)>,
        dir: String,
        copy: bool,
        cx: &mut Context<Self>,
    ) {
        if copy {
            let plans = items
                .into_iter()
                .map(|(path, is_dir)| (path, is_dir, dir.clone()))
                .collect();
            self.run_copies(plans, cx);
        } else {
            let moves = items
                .into_iter()
                .filter(|(path, _)| parent_of(path) != dir)
                .map(|(path, is_dir)| {
                    let to = join(&dir, name_of(&path));
                    (path, is_dir, to)
                })
                .collect();
            self.run_moves(moves, cx);
        }
    }

    /// Duplicate entries beside themselves ("name copy").
    pub(super) fn duplicate(&mut self, items: Vec<(String, bool)>, cx: &mut Context<Self>) {
        let plans = items
            .into_iter()
            .map(|(path, is_dir)| {
                let dir = parent_of(&path).to_string();
                (path, is_dir, dir)
            })
            .collect();
        self.run_copies(plans, cx);
    }

    /// Renames and moves: `(from, is_dir, to)`. Nothing is replaced — an
    /// occupied destination is reported instead.
    fn run_moves(&mut self, moves: Vec<(String, bool, String)>, cx: &mut Context<Self>) {
        if moves.is_empty() {
            return;
        }
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let rename = moves.len() == 1 && parent_of(&moves[0].0) == parent_of(&moves[0].2);
        self.set_status(Some(if rename {
            "Renaming…".into()
        } else {
            format!("Moving {}…", plural(moves.len(), "item"))
        }));
        let task = cx.spawn(async move |this, cx| {
            let mut done = Vec::new();
            let mut failure: Option<SharedString> = None;
            for (from, is_dir, to) in moves {
                let mut params = context.params(&from);
                params.insert("to".into(), to.clone().into());
                match engine
                    .client()
                    .call(
                        methods::MOVE_WORKSPACE_ENTRY,
                        serde_json::Value::Object(params),
                    )
                    .await
                {
                    Ok(_) => done.push((from, is_dir, to)),
                    Err(err) if is_conflict(&err) => {
                        let folder = match parent_of(&to) {
                            "" => "the project root".to_string(),
                            dir => format!("“{}”", name_of(dir)),
                        };
                        failure =
                            Some(format!("“{}” already exists in {folder}.", name_of(&to)).into());
                    }
                    Err(err) => failure = Some(rpc_message(&err)),
                }
            }
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let mut dirs = Vec::new();
                this.selection.clear();
                for (from, _, to) in &done {
                    dirs.push(parent_of(from).to_string());
                    dirs.push(parent_of(to).to_string());
                    this.entry_moved(from, to, cx);
                    this.selection.toggle(to);
                    this.expand_ancestors(to);
                }
                if let Some(last) = done.last() {
                    this.reveal_pending = Some(last.2.clone());
                }
                this.reload_dirs(dirs, cx);
                match failure {
                    Some(error) => this.set_error(error),
                    None if rename => this.set_status(None),
                    None => this.set_status(Some(format!("Moved {}", plural(done.len(), "item")))),
                }
                cx.notify();
            })
            .ok();
        });
        self.keep_task(task);
    }

    /// Copies: `(source, is_dir, destination folder)`. Into the source's own
    /// folder (or onto a taken name) the copy is named "… copy".
    fn run_copies(&mut self, plans: Vec<(String, bool, String)>, cx: &mut Context<Self>) {
        if plans.is_empty() {
            return;
        }
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let taken: Vec<HashSet<String>> = plans
            .iter()
            .map(|(_, _, dir)| self.taken_names(dir))
            .collect();
        self.set_status(Some(format!("Copying {}…", plural(plans.len(), "item"))));
        let task = cx.spawn(async move |this, cx| {
            let mut done = Vec::new();
            let mut failure: Option<SharedString> = None;
            for ((from, is_dir, dir), mut taken) in plans.into_iter().zip(taken) {
                let name = name_of(&from).to_string();
                let beside = parent_of(&from) == dir;
                let mut n = if beside || taken.contains(&name.to_lowercase()) {
                    free_name(&taken, |n| copy_name(&name, is_dir, n)).1
                } else {
                    0
                };
                let mut attempts = 0;
                loop {
                    let candidate = if n == 0 {
                        name.clone()
                    } else {
                        copy_name(&name, is_dir, n)
                    };
                    let to = join(&dir, &candidate);
                    let mut params = context.params(&from);
                    params.insert("to".into(), to.clone().into());
                    match engine
                        .client()
                        .call(
                            methods::COPY_WORKSPACE_ENTRY,
                            serde_json::Value::Object(params),
                        )
                        .await
                    {
                        Ok(_) => {
                            done.push(to);
                            break;
                        }
                        Err(err) if is_conflict(&err) && attempts < 20 => {
                            attempts += 1;
                            taken.insert(candidate.to_lowercase());
                            n = free_name(&taken, |n| copy_name(&name, is_dir, n)).1;
                        }
                        Err(err) => {
                            failure = Some(rpc_message(&err));
                            break;
                        }
                    }
                }
            }
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.selection.clear();
                let mut dirs = Vec::new();
                for to in &done {
                    dirs.push(parent_of(to).to_string());
                    this.selection.toggle(to);
                    this.expand_ancestors(to);
                }
                if let Some(last) = done.last() {
                    this.reveal_pending = Some(last.clone());
                }
                this.reload_dirs(dirs, cx);
                match failure {
                    Some(error) => this.set_error(error),
                    None => this.set_status(Some(format!("Copied {}", plural(done.len(), "item")))),
                }
                cx.notify();
            })
            .ok();
        });
        self.keep_task(task);
    }

    // ---- delete ----

    /// Ask before deleting (the Trash by default).
    pub(super) fn request_delete(
        &mut self,
        items: Vec<(String, bool)>,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() || self.context.is_none() {
            return;
        }
        self.confirm = Some(super::menu::DeleteConfirm {
            items,
            permanent,
            note: None,
        });
        window.focus(&self.confirm_focus, cx);
        cx.notify();
    }

    pub(super) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        window.focus(&self.tree_focus, cx);
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let permanent = confirm.permanent;
        self.set_status(Some(format!(
            "{} {}…",
            if permanent {
                "Deleting"
            } else {
                "Moving to the Trash:"
            },
            plural(confirm.items.len(), "item")
        )));
        let task = cx.spawn(async move |this, cx| {
            let mut done = Vec::new();
            let mut failure: Option<(SharedString, bool)> = None;
            let mut remaining = confirm.items.clone();
            while let Some((path, is_dir)) = remaining.first().cloned() {
                let mut params = context.params(&path);
                params.insert("permanent".into(), permanent.into());
                match engine
                    .client()
                    .call(
                        methods::DELETE_WORKSPACE_ENTRY,
                        serde_json::Value::Object(params),
                    )
                    .await
                {
                    Ok(_) => {
                        done.push((path, is_dir));
                        remaining.remove(0);
                    }
                    Err(err) => {
                        // No Trash here: offer to delete what is left for good.
                        let no_trash = !permanent && err.to_string().contains("Trash");
                        failure = Some((rpc_message(&err), no_trash));
                        break;
                    }
                }
            }
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let mut dirs = Vec::new();
                for (path, _) in &done {
                    dirs.push(parent_of(path).to_string());
                    this.entry_removed(path);
                }
                this.reload_dirs(dirs, cx);
                match failure {
                    Some((error, true)) => {
                        this.set_status(None);
                        this.confirm = Some(super::menu::DeleteConfirm {
                            items: remaining,
                            permanent: true,
                            note: Some(error),
                        });
                        this.confirm_focus_pending = true;
                    }
                    Some((error, false)) => this.set_error(error),
                    None if permanent => {
                        this.set_status(Some(format!("Deleted {}", plural(done.len(), "item"))))
                    }
                    None => this.set_status(Some(format!(
                        "Moved {} to the Trash",
                        plural(done.len(), "item")
                    ))),
                }
                this.rebuild_rows();
                cx.notify();
            })
            .ok();
        });
        self.keep_task(task);
    }

    // ---- import ----

    /// Copy local files (dropped from Finder, or pasted) into `dir` on the
    /// host. A name already taken there gets a number ("a 2.txt").
    pub(super) fn import(&mut self, paths: Vec<PathBuf>, dir: String, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let Some((engine, context)) = self.engine_and_context(cx) else {
            return;
        };
        let generation = self.generation;
        let mut taken = self.taken_names(&dir);
        self.set_status(Some("Preparing import…".into()));
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let planned = cx
                .background_executor()
                .spawn(async move { plan_import(&paths) })
                .await;
            let (plans, total) = match planned {
                Ok(planned) => planned,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.set_error(error.into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let count: usize = plans.iter().map(|p| p.items.len().max(1)).sum();
            let mut sent = 0u64;
            let mut finished = 0usize;
            let mut imported = Vec::new();
            let mut failure: Option<SharedString> = None;
            'plans: for plan in plans {
                // Claim a free top-level name (the folder, or the file's
                // first slice) — a conflict moves on to the next number.
                let mut n = if taken.contains(&plan.name.to_lowercase()) {
                    free_name(&taken, |n| numbered_name(&plan.name, plan.is_dir, n)).1
                } else {
                    1
                };
                let top = loop {
                    let name = numbered_name(&plan.name, plan.is_dir, n);
                    let top = join(&dir, &name);
                    let claimed = if plan.is_dir {
                        let mut params = context.params(&top);
                        params.insert("directory".into(), true.into());
                        engine
                            .client()
                            .call(
                                methods::CREATE_WORKSPACE_ENTRY,
                                serde_json::Value::Object(params),
                            )
                            .await
                            .map(|_| ())
                    } else {
                        Ok(())
                    };
                    match claimed {
                        Ok(()) => break top,
                        Err(err) if is_conflict(&err) && n < 50 => {
                            taken.insert(name.to_lowercase());
                            n = free_name(&taken, |n| numbered_name(&plan.name, plan.is_dir, n)).1;
                        }
                        Err(err) => {
                            failure = Some(rpc_message(&err));
                            break 'plans;
                        }
                    }
                };
                taken.insert(name_of(&top).to_lowercase());
                let mut top = top;
                for item in &plan.items {
                    let mut target = if item.rel.is_empty() {
                        top.clone()
                    } else {
                        join(&top, &item.rel)
                    };
                    match &item.source {
                        None => {
                            let mut params = context.params(&target);
                            params.insert("directory".into(), true.into());
                            if let Err(err) = engine
                                .client()
                                .call(
                                    methods::CREATE_WORKSPACE_ENTRY,
                                    serde_json::Value::Object(params),
                                )
                                .await
                            {
                                failure = Some(rpc_message(&err));
                                break 'plans;
                            }
                        }
                        Some((source, size)) => {
                            let mut offset = 0u64;
                            let mut file_n = n;
                            loop {
                                let source_path = source.clone();
                                let at = offset;
                                let slice = cx
                                    .background_executor()
                                    .spawn(async move { read_slice(&source_path, at) })
                                    .await;
                                let slice = match slice {
                                    Ok(slice) => slice,
                                    Err(error) => {
                                        failure = Some(error.into());
                                        break 'plans;
                                    }
                                };
                                let mut params = context.params(&target);
                                params.insert("offset".into(), offset.into());
                                params.insert(
                                    "data".into(),
                                    base64::engine::general_purpose::STANDARD
                                        .encode(&slice)
                                        .into(),
                                );
                                match engine
                                    .client()
                                    .call(
                                        methods::WRITE_WORKSPACE_FILE_CHUNK,
                                        serde_json::Value::Object(params),
                                    )
                                    .await
                                {
                                    Ok(_) => {}
                                    // A loose top-level file whose name was
                                    // taken meanwhile: next number.
                                    Err(err)
                                        if offset == 0
                                            && !plan.is_dir
                                            && is_conflict(&err)
                                            && file_n < 50 =>
                                    {
                                        taken.insert(name_of(&target).to_lowercase());
                                        file_n = free_name(&taken, |n| {
                                            numbered_name(&plan.name, false, n)
                                        })
                                        .1;
                                        target = join(&dir, &numbered_name(&plan.name, false, file_n));
                                        top = target.clone();
                                        continue;
                                    }
                                    Err(err) => {
                                        if offset > 0 {
                                            let mut params = context.params(&target);
                                            params.insert("permanent".into(), true.into());
                                            let _ = engine
                                                .client()
                                                .call(
                                                    methods::DELETE_WORKSPACE_ENTRY,
                                                    serde_json::Value::Object(params),
                                                )
                                                .await;
                                        }
                                        failure = Some(match err {
                                            RpcError::UnknownMethod(_) => {
                                                "This device's engine needs an update to import files."
                                                    .into()
                                            }
                                            other => rpc_message(&other),
                                        });
                                        break 'plans;
                                    }
                                }
                                offset += slice.len() as u64;
                                sent += slice.len() as u64;
                                let progress = format!(
                                    "Importing {} of {} · {} of {}",
                                    (finished + 1).min(count),
                                    count,
                                    super::human_bytes(sent),
                                    super::human_bytes(total)
                                );
                                if this
                                    .update(cx, |this, cx| {
                                        this.set_status(Some(progress));
                                        cx.notify();
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                                if offset >= *size || slice.is_empty() {
                                    break;
                                }
                            }
                        }
                    }
                    finished += 1;
                }
                imported.push(top);
            }
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if !dir.is_empty() {
                    this.expanded.insert(dir.clone());
                }
                this.selection.clear();
                for path in &imported {
                    this.selection.toggle(path);
                }
                if let Some(last) = imported.last() {
                    this.reveal_pending = Some(last.clone());
                }
                this.reload_dirs([dir.clone()], cx);
                match failure {
                    Some(error) => this.set_error(error),
                    None => this.set_status(Some(format!(
                        "Imported {}",
                        plural(imported.len(), "item")
                    ))),
                }
                cx.notify();
            })
            .ok();
        });
        self.keep_task(task);
    }

    // ---- path actions ----

    /// The entry's path on its host (`~` expanded when the host is this
    /// machine).
    pub(super) fn absolute_path(&self, path: &str) -> String {
        let Some(context) = &self.context else {
            return path.to_string();
        };
        let root = if context.target.is_none() {
            expand_home(&context.cwd)
        } else {
            context.cwd.clone()
        };
        if path.is_empty() {
            root
        } else {
            format!("{}/{path}", root.trim_end_matches('/'))
        }
    }

    /// Whether the checkout lives on this machine (Finder verbs apply).
    pub(super) fn is_local(&self) -> bool {
        self.context.as_ref().is_some_and(|c| c.target.is_none())
    }

    pub(super) fn copy_path(
        &mut self,
        items: &[(String, bool)],
        relative: bool,
        cx: &mut Context<Self>,
    ) {
        let text = items
            .iter()
            .map(|(path, _)| {
                if relative {
                    path.clone()
                } else {
                    self.absolute_path(path)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status(Some(if relative {
            "Copied relative path".into()
        } else {
            "Copied path".into()
        }));
        cx.notify();
    }

    pub(super) fn reveal_in_finder(&self, path: &str, cx: &mut Context<Self>) {
        if self.is_local() {
            cx.reveal_path(std::path::Path::new(&self.absolute_path(path)));
        }
    }

    pub(super) fn open_with_default_app(&self, path: &str, cx: &mut Context<Self>) {
        if self.is_local() {
            cx.open_with_system(std::path::Path::new(&self.absolute_path(path)));
        }
    }

    pub(super) fn mention(&mut self, items: &[(String, bool)], cx: &mut Context<Self>) {
        for (path, is_dir) in items {
            cx.emit(FilesEvent::Mention {
                path: path.clone(),
                is_dir: *is_dir,
            });
        }
    }

    /// Lowercased names already in `dir` (empty when it was never listed).
    pub(super) fn taken_names(&self, dir: &str) -> HashSet<String> {
        self.dirs
            .get(dir)
            .map(|d| d.entries.iter().map(|e| e.name.to_lowercase()).collect())
            .unwrap_or_default()
    }

    /// Expand every folder above `path` (so a revealed row is visible).
    pub(super) fn expand_ancestors(&mut self, path: &str) {
        let mut dir = parent_of(path);
        while !dir.is_empty() {
            self.expanded.insert(dir.to_string());
            dir = parent_of(dir);
        }
    }
}

/// `~` / `~/…` → this machine's home (the engine's own rule for chat cwds).
fn expand_home(cwd: &str) -> String {
    match cwd.strip_prefix('~') {
        Some("") => cypher_env::home_dir().to_string_lossy().into_owned(),
        Some(rest) if rest.starts_with('/') => cypher_env::home_dir()
            .join(&rest[1..])
            .to_string_lossy()
            .into_owned(),
        _ => cwd.to_string(),
    }
}

/// `path`'s ancestors and itself, outermost first ("a", "a/b", "a/b/c").
fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = String::new();
    for part in path.split('/') {
        at = join(&at, part);
        out.push(at.clone());
    }
    out
}

fn read_slice(path: &std::path::Path, offset: u64) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(IMPORT_CHUNK);
    file.take(IMPORT_CHUNK as u64)
        .read_to_end(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(buf)
}

/// File paths on the system clipboard (a Finder "Copy").
fn external_clipboard_paths(cx: &gpui::App) -> Vec<PathBuf> {
    cx.read_from_clipboard()
        .map(|item| {
            item.entries()
                .iter()
                .filter_map(|entry| match entry {
                    ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                    _ => None,
                })
                .flatten()
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_and_import_names_follow_finder() {
        assert_eq!(copy_name("a.txt", false, 1), "a copy.txt");
        assert_eq!(copy_name("a.txt", false, 3), "a copy 3.txt");
        assert_eq!(copy_name("v1.2", true, 1), "v1.2 copy");
        assert_eq!(copy_name(".env", false, 2), ".env copy 2");
        assert_eq!(numbered_name("a.txt", false, 1), "a.txt");
        assert_eq!(numbered_name("a.txt", false, 2), "a 2.txt");
        let taken: HashSet<String> = ["a copy.txt".into(), "a copy 2.txt".into()].into();
        assert_eq!(
            free_name(&taken, |n| copy_name("a.txt", false, n)),
            ("a copy 3.txt".to_string(), 3)
        );
        assert_eq!(ancestors("a/b/c"), ["a", "a/b", "a/b/c"]);
    }

    #[test]
    fn import_plans_walk_folders_and_skip_links_and_git() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("pkg/src")).unwrap();
        std::fs::create_dir(root.join("pkg/.git")).unwrap();
        std::fs::write(root.join("pkg/.git/HEAD"), "ref").unwrap();
        std::fs::write(root.join("pkg/src/lib.rs"), "fn x() {}").unwrap();
        std::fs::write(root.join("note.md"), "hi").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", root.join("pkg/etc")).unwrap();
        let (plans, total) = plan_import(&[root.join("pkg"), root.join("note.md")]).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].name, "pkg");
        assert!(plans[0].is_dir);
        let rels: Vec<_> = plans[0].items.iter().map(|i| i.rel.as_str()).collect();
        assert_eq!(rels, ["src", "src/lib.rs"]);
        assert!(plans[0].items[0].source.is_none());
        assert_eq!(plans[1].items[0].rel, "");
        assert_eq!(total, 9 + 2);
    }
}
