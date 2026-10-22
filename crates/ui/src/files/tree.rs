//! The tree column: flattened rows over the loaded folders, the multi-row
//! selection, git status marks, the inline name field (rename / new entry),
//! and row rendering with its mouse, drag and drop wiring.

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, Entity, ExternalPaths, MouseButton, MouseDownEvent, Pixels, Point,
    SharedString, Subscription, Window, div, prelude::*, px,
};

use cypher_proto::{CheckoutDiff, WorkspaceFileEntry};

use super::{DirState, FilesPanel, INDENT, ROW_HEIGHT, ROW_RADIUS, preview};
use crate::kit::icons::{self, icon};
use crate::kit::theme::Theme;
use crate::widgets::text_input::TextInput;

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
    /// The inline "new file/folder" field's row (`Some(directory)`).
    pub create: Option<bool>,
}

impl TreeRow {
    /// A real file or folder (not a placeholder or the new-entry field).
    pub fn is_entry(&self) -> bool {
        self.note.is_none() && self.create.is_none()
    }
}

pub(super) fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

pub(super) fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |slash| &path[..slash])
}

pub(super) fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `path` is `ancestor` or lies inside it.
pub(super) fn within(ancestor: &str, path: &str) -> bool {
    ancestor.is_empty()
        || path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `path` with its `from` prefix swapped for `to` (a renamed ancestor).
pub(super) fn rebase(path: &str, from: &str, to: &str) -> Option<String> {
    if path == from {
        return Some(to.to_string());
    }
    let rest = path.strip_prefix(from)?.strip_prefix('/')?;
    Some(join(to, rest))
}

/// What flattening needs beyond the loaded folders.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct FlattenOptions<'a> {
    pub show_hidden: bool,
    /// The new-entry field: `(parent folder, directory)`.
    pub creating: Option<(&'a str, bool)>,
}

/// Flatten the loaded directories into rows, depth-first, following
/// `expanded`. Pure so the tree order is testable without a shell.
pub(super) fn flatten_tree(
    dirs: &HashMap<String, DirState>,
    expanded: &HashSet<String>,
    options: FlattenOptions<'_>,
) -> Vec<TreeRow> {
    fn walk(
        dir_path: &str,
        depth: usize,
        dirs: &HashMap<String, DirState>,
        expanded: &HashSet<String>,
        options: FlattenOptions<'_>,
        out: &mut Vec<TreeRow>,
    ) {
        let note = |text: &str| TreeRow {
            path: format!("{dir_path}\u{0}note"),
            name: String::new(),
            is_dir: false,
            depth,
            expanded: false,
            loading: false,
            note: Some(text.to_string().into()),
            create: None,
        };
        if let Some((parent, directory)) = options.creating
            && parent == dir_path
        {
            out.push(TreeRow {
                path: format!("{dir_path}\u{0}create"),
                name: String::new(),
                is_dir: directory,
                depth,
                expanded: false,
                loading: false,
                note: None,
                create: Some(directory),
            });
        }
        let Some(dir) = dirs.get(dir_path) else {
            return;
        };
        if let Some(error) = &dir.error {
            out.push(note(error));
            return;
        }
        if dir.loading && !dir.loaded {
            out.push(note("Loading…"));
            return;
        }
        let visible: Vec<&WorkspaceFileEntry> = dir
            .entries
            .iter()
            .filter(|entry| options.show_hidden || !entry.name.starts_with('.'))
            .collect();
        if dir.loaded && visible.is_empty() && options.creating.is_none_or(|(p, _)| p != dir_path) {
            out.push(note("Empty folder"));
            return;
        }
        for entry in visible {
            let path = join(dir_path, &entry.name);
            let is_expanded = entry.is_dir && expanded.contains(&path);
            out.push(TreeRow {
                path: path.clone(),
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                depth,
                expanded: is_expanded,
                loading: entry.is_dir && dirs.get(&path).is_some_and(|d| d.loading && !d.loaded),
                note: None,
                create: None,
            });
            if is_expanded {
                walk(&path, depth + 1, dirs, expanded, options, out);
            }
        }
        if dir.truncated {
            out.push(note("… listing cut at 1000 entries"));
        }
    }
    let mut rows = Vec::new();
    walk("", 0, dirs, expanded, options, &mut rows);
    rows
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// The picked rows: an ordered set plus the shift-range anchor and the
/// keyboard cursor. Pure over row lists.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Selection {
    pub items: Vec<String>,
    pub anchor: Option<String>,
    pub cursor: Option<String>,
}

impl Selection {
    pub fn contains(&self, path: &str) -> bool {
        self.items.iter().any(|p| p == path)
    }

    pub fn single(&mut self, path: &str) {
        self.items = vec![path.to_string()];
        self.anchor = Some(path.to_string());
        self.cursor = Some(path.to_string());
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// ⌘-click: add or remove one row, keeping the rest.
    pub fn toggle(&mut self, path: &str) {
        if let Some(ix) = self.items.iter().position(|p| p == path) {
            self.items.remove(ix);
        } else {
            self.items.push(path.to_string());
        }
        self.anchor = Some(path.to_string());
        self.cursor = Some(path.to_string());
    }

    /// ⇧-click / ⇧-arrow: every entry row between the anchor and `path`.
    pub fn extend_to(&mut self, rows: &[TreeRow], path: &str) {
        let entries: Vec<&TreeRow> = rows.iter().filter(|r| r.is_entry()).collect();
        let at = |p: &str| entries.iter().position(|r| r.path == p);
        let Some(end) = at(path) else {
            return;
        };
        let start = self.anchor.as_deref().and_then(at).unwrap_or(end);
        let (lo, hi) = (start.min(end), start.max(end));
        self.items = entries[lo..=hi].iter().map(|r| r.path.clone()).collect();
        if self.anchor.is_none() {
            self.anchor = Some(path.to_string());
        }
        self.cursor = Some(path.to_string());
    }

    /// The selected rows that are still visible, in row order (what an
    /// action acts on), each with whether it is a folder.
    pub fn resolve(&self, rows: &[TreeRow]) -> Vec<(String, bool)> {
        rows.iter()
            .filter(|r| r.is_entry() && self.contains(&r.path))
            .map(|r| (r.path.clone(), r.is_dir))
            .collect()
    }

    /// Paths under a renamed/moved entry follow it.
    pub fn rebase(&mut self, from: &str, to: &str) {
        let swap = |p: &mut String| {
            if let Some(next) = rebase(p, from, to) {
                *p = next;
            }
        };
        self.items.iter_mut().for_each(swap);
        self.anchor.as_mut().map(swap);
        self.cursor.as_mut().map(swap);
    }
}

/// The entry row `delta` steps from the cursor (clamped), for arrow keys.
pub(super) fn step(rows: &[TreeRow], cursor: Option<&str>, delta: isize) -> Option<String> {
    let entries: Vec<&TreeRow> = rows.iter().filter(|r| r.is_entry()).collect();
    if entries.is_empty() {
        return None;
    }
    let next = match cursor.and_then(|c| entries.iter().position(|r| r.path == c)) {
        None if delta >= 0 => 0,
        None => entries.len() - 1,
        Some(at) => (at as isize + delta).clamp(0, entries.len() as isize - 1) as usize,
    };
    Some(entries[next].path.clone())
}

// ---------------------------------------------------------------------------
// Git marks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GitMark {
    Modified,
    Added,
    Renamed,
    Conflict,
}

impl GitMark {
    fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Renamed => "R",
            Self::Conflict => "!",
        }
    }

    fn color(self, theme: &Theme) -> gpui::Hsla {
        match self {
            Self::Modified => theme.warning,
            Self::Added | Self::Renamed => theme.success,
            Self::Conflict => theme.danger,
        }
    }
}

/// The checkout's uncommitted changes by path, and the folders holding any.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct GitMarks {
    pub files: HashMap<String, GitMark>,
    pub dirs: HashSet<String>,
}

impl GitMarks {
    pub fn from_diff(diff: Option<&CheckoutDiff>) -> Self {
        let mut marks = Self::default();
        let Some(diff) = diff else {
            return marks;
        };
        for file in &diff.files {
            let mark = match file.status.as_str() {
                "deleted" => continue,
                "added" | "copied" => GitMark::Added,
                "renamed" => GitMark::Renamed,
                "unmerged" => GitMark::Conflict,
                _ => GitMark::Modified,
            };
            let mut dir = parent_of(&file.path);
            while !dir.is_empty() {
                if !marks.dirs.insert(dir.to_string()) {
                    break;
                }
                dir = parent_of(dir);
            }
            marks.files.insert(file.path.clone(), mark);
        }
        marks
    }
}

// ---------------------------------------------------------------------------
// Inline name field
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EditTarget {
    Rename { path: String, is_dir: bool },
    Create { parent: String, directory: bool },
}

/// The inline name field over a row (rename) or in the new-entry row.
pub(super) struct InlineEdit {
    pub target: EditTarget,
    pub input: Entity<TextInput>,
    pub error: Option<SharedString>,
    pub focus_pending: bool,
    pub _subs: Vec<Subscription>,
}

/// Why `name` cannot name an entry, if it cannot. `nested` allows `a/b`
/// (a new entry creates its missing folders).
pub(super) fn name_problem(name: &str, nested: bool) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("A name is required.");
    }
    if name.contains('\0') || name.contains('\\') && cfg!(windows) {
        return Some("That name has a character files cannot use.");
    }
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() > 1 && !nested {
        return Some("Names cannot contain “/”.");
    }
    for part in parts {
        if part.is_empty() || part == "." || part == ".." {
            return Some("That is not a valid name.");
        }
        if part.eq_ignore_ascii_case(".git") {
            return Some("Git metadata cannot be edited here.");
        }
        if part.len() > 255 {
            return Some("That name is too long.");
        }
    }
    None
}

/// The part of a name a rename selects first: the stem, not the extension.
pub(super) fn stem_range(name: &str, is_dir: bool) -> std::ops::Range<usize> {
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => 0..dot,
        _ => 0..name.len(),
    }
}

// ---------------------------------------------------------------------------
// Drag payload
// ---------------------------------------------------------------------------

/// Rows being dragged inside the tree (move; ⌥ copies).
#[derive(Clone)]
pub(super) struct FileDrag {
    pub panel: gpui::EntityId,
    pub items: Vec<(String, bool)>,
}

impl FileDrag {
    pub fn ghost(
        &self,
        offset: Point<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Entity<FileDragGhost> {
        cx.stop_propagation();
        let label: SharedString = match self.items.as_slice() {
            [(path, _)] => name_of(path).to_string().into(),
            items => format!("{} items", items.len()).into(),
        };
        let glyph = match self.items.as_slice() {
            [(_, true)] => icons::FOLDER,
            [(path, false)] => preview::file_icon(path),
            _ => icons::FOLDER_WITH_FILES,
        };
        cx.new(|_| FileDragGhost {
            label,
            glyph,
            offset,
        })
    }

    /// Whether dropping into `dir` would do anything sensible: never into
    /// a dragged folder (or below it), never back into the same folder.
    pub fn fits(&self, dir: &str) -> bool {
        self.items
            .iter()
            .all(|(path, is_dir)| !(*is_dir && within(path, dir)) && parent_of(path) != dir)
    }
}

pub(super) struct FileDragGhost {
    label: SharedString,
    glyph: &'static str,
    offset: Point<Pixels>,
}

impl Render for FileDragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let (x, y) = (f32::from(self.offset.x), f32::from(self.offset.y));
        div()
            .pl(px(x - x.min(16.0)))
            .pt(px(y - y.min(ROW_HEIGHT / 2.0)))
            .child(
                div()
                    .h(px(ROW_HEIGHT))
                    .max_w(px(240.0))
                    .px(px(8.0))
                    .rounded(px(ROW_RADIUS))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border_strong)
                    .shadow_md()
                    .opacity(0.92)
                    .child(icon(self.glyph).size(px(12.0)).text_color(theme.text_muted))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .child(self.label.clone()),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------
// Row rendering
// ---------------------------------------------------------------------------

/// Per-frame facts every row reads.
pub(super) struct RowContext {
    pub panel: Entity<FilesPanel>,
    pub panel_id: gpui::EntityId,
    pub open: Option<String>,
    pub selection: Selection,
    /// The selection resolved against the rows (path, is folder).
    pub picked: Vec<(String, bool)>,
    pub dirty: HashSet<String>,
    pub cut: HashSet<String>,
    pub git: GitMarks,
    pub edit: Option<(EditTarget, Entity<TextInput>, Option<SharedString>)>,
    pub tree_focused: bool,
}

pub(super) fn render_row(ix: usize, row: &TreeRow, ctx: &RowContext, theme: &Theme) -> AnyElement {
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
    let editing = match (&ctx.edit, &row.create) {
        (Some((EditTarget::Create { .. }, input, error)), Some(_)) => Some((input, error)),
        (Some((EditTarget::Rename { path, .. }, input, error)), None) if *path == row.path => {
            Some((input, error))
        }
        _ => None,
    };
    let glyph = if row.is_dir {
        icons::FOLDER
    } else if row.create.is_some() {
        icons::DOCUMENT
    } else {
        preview::file_icon(&row.name)
    };
    let chevron = row.is_dir.then_some(if row.expanded {
        icons::ALT_ARROW_DOWN
    } else {
        icons::ALT_ARROW_RIGHT
    });
    let lead = |el: gpui::Div| {
        el.child(
            div()
                .flex_none()
                .size(px(12.0))
                .flex()
                .items_center()
                .justify_center()
                .when_some(chevron.filter(|_| row.create.is_none()), |el, chevron| {
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
    };

    if let Some((input, error)) = editing {
        let field = div()
            .flex_1()
            .min_w_0()
            .h(px(20.0))
            .px(px(4.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(if error.is_some() {
                theme.danger
            } else {
                theme.accent
            })
            .bg(theme.bg)
            .flex()
            .items_center()
            .overflow_hidden()
            .child(input.clone());
        let panel = ctx.panel.clone();
        let row_el = lead(
            div()
                .w_full()
                .h(px(ROW_HEIGHT))
                .pl(px(indent))
                .pr(px(4.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0)),
        )
        .child(field);
        return div()
            .id(("files-edit", ix))
            .w_full()
            .relative()
            // Clicks in the field stay there (the background would focus
            // the tree, committing the name).
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    panel.update(cx, |panel, cx| panel.cancel_edit(window, cx));
                }
            })
            .child(row_el)
            .when_some(error.clone(), |el, error| {
                el.child(
                    div()
                        .absolute()
                        .top(px(ROW_HEIGHT))
                        .left(px(indent + 30.0))
                        .right(px(4.0))
                        .px(px(6.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .bg(theme.danger)
                        .text_size(px(11.0))
                        .text_color(theme.bg)
                        .child(error),
                )
            })
            .into_any_element();
    }

    let path = row.path.clone();
    let is_dir = row.is_dir;
    let selected = ctx.selection.contains(&row.path);
    let open = !is_dir && ctx.open.as_deref() == Some(row.path.as_str());
    let cursor = ctx.tree_focused && ctx.selection.cursor.as_deref() == Some(row.path.as_str());
    let dirty = ctx.dirty.contains(&row.path);
    let cut = ctx.cut.contains(&row.path);
    let mark = ctx.git.files.get(&row.path).copied();
    let dir_marked = is_dir && ctx.git.dirs.contains(&row.path);
    let name_color = match mark {
        Some(mark) => mark.color(theme),
        None if dir_marked => theme.warning.opacity(0.85),
        None if selected || open => theme.text,
        None => theme.text.opacity(0.85),
    };
    let drop_dir = if is_dir {
        row.path.clone()
    } else {
        parent_of(&row.path).to_string()
    };

    let click_panel = ctx.panel.clone();
    let click_path = path.clone();
    let menu_panel = ctx.panel.clone();
    let menu_path = path.clone();
    let drop_panel = ctx.panel.clone();
    let drop_target = drop_dir.clone();
    let import_panel = ctx.panel.clone();
    let import_target = drop_dir.clone();
    let can_drop_target = drop_dir.clone();
    let panel_id = ctx.panel_id;
    let drag_items = if selected && ctx.picked.len() > 1 {
        ctx.picked.clone()
    } else {
        vec![(row.path.clone(), is_dir)]
    };
    let drag = FileDrag {
        panel: ctx.panel_id,
        items: drag_items,
    };

    lead(
        div()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(indent))
            .pr(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0)),
    )
    .id(("files-row", ix))
    .cursor_pointer()
    .rounded(px(ROW_RADIUS))
    .when(cut, |el| el.opacity(0.5))
    .when(selected, |el| {
        el.bg(if ctx.tree_focused {
            crate::kit::theme::wash(0.12)
        } else {
            crate::kit::theme::wash(0.08)
        })
    })
    .when(!selected && open, |el| el.bg(crate::kit::theme::wash(0.05)))
    .when(!selected, |el| {
        el.hover(|s| s.bg(crate::kit::theme::wash(0.05)))
    })
    .when(cursor && !selected, |el| {
        el.border_1().border_color(theme.border_strong)
    })
    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
    .on_click(move |event, window, cx| {
        // The tree background clears the selection on click — not this one.
        cx.stop_propagation();
        let modifiers = event.modifiers();
        let double = event.click_count() >= 2;
        click_panel.update(cx, |panel, cx| {
            panel.click_row(&click_path, is_dir, modifiers, double, window, cx)
        });
    })
    .on_mouse_down(
        MouseButton::Right,
        move |event: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            menu_panel.update(cx, |panel, cx| {
                panel.open_row_menu(&menu_path, event.position, window, cx)
            });
        },
    )
    .on_drag(drag, |drag: &FileDrag, offset, window, cx| {
        drag.ghost(offset, window, cx)
    })
    .when(is_dir, |el| {
        el.drag_over::<FileDrag>(move |style, drag, _, _| {
            if drag.panel == panel_id && drag.fits(&can_drop_target) {
                style.bg(crate::kit::theme::wash(0.16))
            } else {
                style
            }
        })
        .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(crate::kit::theme::wash(0.16)))
    })
    .on_drop(move |drag: &FileDrag, window, cx| {
        if drag.panel != panel_id || !drag.fits(&drop_target) {
            return;
        }
        let copy = window.modifiers().alt;
        let items = drag.items.clone();
        let target = drop_target.clone();
        drop_panel.update(cx, |panel, cx| panel.transfer(items, target, copy, cx));
    })
    .on_drop(move |paths: &ExternalPaths, _, cx| {
        let paths = paths.paths().to_vec();
        let target = import_target.clone();
        import_panel.update(cx, |panel, cx| panel.import(paths, target, cx));
    })
    .child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(px(12.0))
            .text_color(name_color)
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
    .when_some(mark, |el, mark| {
        el.child(
            div()
                .flex_none()
                .w(px(10.0))
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(mark.color(theme))
                .child(SharedString::from(mark.letter())),
        )
    })
    .into_any_element()
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
                    size: None,
                    modified_ms: None,
                })
                .collect(),
            truncated: false,
            loading: false,
            loaded,
            error: None,
        }
    }

    fn paths(rows: &[TreeRow]) -> Vec<&str> {
        rows.iter().map(|r| r.path.as_str()).collect()
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
        let rows = flatten_tree(&dirs, &expanded, FlattenOptions::default());
        assert_eq!(paths(&rows), ["src", "Cargo.toml"]);
        expanded.insert("src".into());
        let rows = flatten_tree(&dirs, &expanded, FlattenOptions::default());
        assert_eq!(paths(&rows), ["src", "src/lib.rs", "Cargo.toml"]);
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
        let rows = flatten_tree(&dirs, &expanded, FlattenOptions::default());
        assert_eq!(rows[1].note.as_deref(), Some("Loading…"));
        assert!(rows[0].loading);
        assert_eq!(rows[3].note.as_deref(), Some("Empty folder"));
        let mut truncated = dir(&[("x", false)], true);
        truncated.truncated = true;
        dirs.insert(String::new(), truncated);
        let rows = flatten_tree(&dirs, &HashSet::new(), FlattenOptions::default());
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
    fn hidden_entries_and_the_new_entry_row() {
        let mut dirs = HashMap::new();
        dirs.insert(
            String::new(),
            dir(&[("src", true), (".env", false), ("a.txt", false)], true),
        );
        dirs.insert("src".into(), dir(&[], true));
        let expanded: HashSet<String> = ["src".to_string()].into();
        let shown = flatten_tree(
            &dirs,
            &expanded,
            FlattenOptions {
                show_hidden: true,
                creating: None,
            },
        );
        assert_eq!(paths(&shown), ["src", "src\0note", ".env", "a.txt"]);
        let hidden = flatten_tree(&dirs, &expanded, FlattenOptions::default());
        assert_eq!(paths(&hidden), ["src", "src\0note", "a.txt"]);
        // The new-entry row leads its folder and replaces "Empty folder".
        let creating = flatten_tree(
            &dirs,
            &expanded,
            FlattenOptions {
                show_hidden: false,
                creating: Some(("src", true)),
            },
        );
        assert_eq!(paths(&creating), ["src", "src\0create", "a.txt"]);
        assert_eq!(creating[1].create, Some(true));
        assert_eq!(creating[1].depth, 1);
        assert!(!creating[1].is_entry());
    }

    fn rows(paths: &[&str]) -> Vec<TreeRow> {
        paths
            .iter()
            .map(|p| TreeRow {
                path: p.to_string(),
                name: name_of(p).to_string(),
                is_dir: false,
                depth: 0,
                expanded: false,
                loading: false,
                note: None,
                create: None,
            })
            .collect()
    }

    #[test]
    fn selection_singles_toggles_and_ranges() {
        let rows = rows(&["a", "b", "c", "d"]);
        let mut sel = Selection::default();
        sel.single("b");
        sel.extend_to(&rows, "d");
        assert_eq!(sel.items, ["b", "c", "d"]);
        sel.extend_to(&rows, "a");
        assert_eq!(sel.items, ["a", "b"]);
        sel.toggle("d");
        assert_eq!(sel.items, ["a", "b", "d"]);
        sel.toggle("a");
        assert_eq!(sel.items, ["b", "d"]);
        assert_eq!(
            sel.resolve(&rows),
            [("b".to_string(), false), ("d".to_string(), false)]
        );
        assert_eq!(step(&rows, Some("b"), 1).as_deref(), Some("c"));
        assert_eq!(step(&rows, Some("d"), 1).as_deref(), Some("d"));
        assert_eq!(step(&rows, None, -1).as_deref(), Some("d"));
    }

    #[test]
    fn paths_rebase_under_a_renamed_folder() {
        assert_eq!(
            rebase("src/a.rs", "src", "lib").as_deref(),
            Some("lib/a.rs")
        );
        assert_eq!(rebase("src", "src", "lib").as_deref(), Some("lib"));
        assert_eq!(rebase("src2/a.rs", "src", "lib"), None);
        let mut sel = Selection::default();
        sel.single("src/a.rs");
        sel.rebase("src", "lib");
        assert_eq!(sel.items, ["lib/a.rs"]);
        assert_eq!(sel.cursor.as_deref(), Some("lib/a.rs"));
        assert!(within("", "x"));
        assert!(within("a", "a/b"));
        assert!(!within("a", "ab"));
        assert_eq!(parent_of("a/b/c"), "a/b");
        assert_eq!(parent_of("c"), "");
    }

    #[test]
    fn git_marks_cover_files_and_their_folders() {
        let diff = CheckoutDiff {
            checkout_id: "c".into(),
            device_id: "d".into(),
            cwd: "/r".into(),
            patch: String::new(),
            files: vec![
                cypher_proto::DiffFileSummary {
                    path: "src/ui/a.rs".into(),
                    old_path: None,
                    status: "modified".into(),
                    additions: 1,
                    deletions: 0,
                    binary: false,
                },
                cypher_proto::DiffFileSummary {
                    path: "new.txt".into(),
                    old_path: None,
                    status: "added".into(),
                    additions: 1,
                    deletions: 0,
                    binary: false,
                },
                cypher_proto::DiffFileSummary {
                    path: "gone.txt".into(),
                    old_path: None,
                    status: "deleted".into(),
                    additions: 0,
                    deletions: 1,
                    binary: false,
                },
            ],
            additions: 2,
            deletions: 1,
            truncated: false,
            checksum: "x".into(),
            updated_at: chrono::Utc::now(),
        };
        let marks = GitMarks::from_diff(Some(&diff));
        assert_eq!(marks.files.get("src/ui/a.rs"), Some(&GitMark::Modified));
        assert_eq!(marks.files.get("new.txt"), Some(&GitMark::Added));
        assert!(!marks.files.contains_key("gone.txt"));
        assert!(marks.dirs.contains("src") && marks.dirs.contains("src/ui"));
        assert_eq!(marks.dirs.len(), 2);
    }

    #[test]
    fn names_are_validated_and_stems_selected() {
        assert!(name_problem("a.txt", false).is_none());
        assert!(name_problem("  ", false).is_some());
        assert!(name_problem("a/b", false).is_some());
        assert!(name_problem("a/b", true).is_none());
        assert!(name_problem("a//b", true).is_some());
        assert!(name_problem("..", false).is_some());
        assert!(name_problem(".GIT", false).is_some());
        assert!(name_problem(&"x".repeat(256), false).is_some());
        assert_eq!(stem_range("main.rs", false), 0..4);
        assert_eq!(stem_range(".env", false), 0..4);
        assert_eq!(stem_range("v1.2", true), 0..4);
    }

    #[test]
    fn drags_never_drop_into_themselves_or_their_own_folder() {
        let drag = FileDrag {
            panel: gpui::EntityId::from(1u64),
            items: vec![("src".into(), true), ("docs/a.md".into(), false)],
        };
        assert!(drag.fits("lib"));
        assert!(!drag.fits("src"));
        assert!(!drag.fits("src/inner"));
        assert!(!drag.fits("docs"));
    }
}
