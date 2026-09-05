//! Client-local layout preference and linear-time split-row projection.
use super::{DiffLine, DiffRow, FileDiff, LineKind};
use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffLayout {
    #[default]
    Unified,
    Split,
}
impl DiffLayout {
    pub const ALL: [Self; 2] = [Self::Unified, Self::Split];
    pub fn label(self) -> &'static str {
        match self {
            Self::Unified => "Unified",
            Self::Split => "Split",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}
impl Side {
    pub fn index(self) -> usize {
        match self {
            Self::Old => 0,
            Self::New => 1,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Old => "Old",
            Self::New => "New",
        }
    }
}
pub struct LayoutState {
    pub layout: DiffLayout,
    dir: PathBuf,
}
impl Global for LayoutState {}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Preference {
    layout: DiffLayout,
}
const FILE_NAME: &str = "diff-view.json";
pub fn load(dir: &Path) -> DiffLayout {
    std::fs::read(dir.join(FILE_NAME))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Preference>(&bytes).ok())
        .unwrap_or_default()
        .layout
}
pub fn init(dir: PathBuf, cx: &mut App) {
    cx.set_global(LayoutState {
        layout: load(&dir),
        dir,
    });
}
pub fn current(cx: &App) -> DiffLayout {
    cx.try_global::<LayoutState>()
        .map(|s| s.layout)
        .unwrap_or_default()
}
fn save(dir: &Path, layout: DiffLayout) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".diff-view-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(
            &serde_json::to_vec(&Preference { layout }).map_err(std::io::Error::other)?,
        )?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, dir.join(FILE_NAME))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
pub fn set(layout: DiffLayout, cx: &mut App) -> std::io::Result<()> {
    let state = cx
        .try_global::<LayoutState>()
        .ok_or_else(|| std::io::Error::other("Diff preferences are not initialized."))?;
    save(&state.dir, layout)?;
    if state.layout != layout {
        cx.global_mut::<LayoutState>().layout = layout;
        cx.refresh_windows();
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub old: Option<u32>,
    pub new: Option<u32>,
}
/// Pair adjacent replacement runs by position, not by speculative similarity.
/// Metadata is excluded from source rows and retained on its original side.
pub fn align(lines: &[DiffLine]) -> Vec<Pair> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == LineKind::Context {
            rows.push(Pair {
                old: Some(i as u32),
                new: Some(i as u32),
            });
            i += 1;
        } else if lines[i].kind == LineKind::Meta {
            rows.push(Pair {
                old: Some(i as u32),
                new: Some(i as u32),
            });
            i += 1;
        } else {
            let mut old = Vec::new();
            let mut new = Vec::new();
            let mut meta = Vec::new();
            let mut previous = LineKind::Context;
            while i < lines.len() && lines[i].kind != LineKind::Context {
                let index = Some(i as u32);
                match lines[i].kind {
                    LineKind::Del => {
                        old.push(index);
                        previous = LineKind::Del;
                    }
                    LineKind::Add => {
                        new.push(index);
                        previous = LineKind::Add;
                    }
                    LineKind::Meta => meta.push(Pair {
                        old: (previous != LineKind::Add).then_some(i as u32),
                        new: (previous != LineKind::Del).then_some(i as u32),
                    }),
                    LineKind::Context => unreachable!(),
                }
                i += 1;
            }
            for j in 0..old.len().max(new.len()) {
                rows.push(Pair {
                    old: old.get(j).copied().flatten(),
                    new: new.get(j).copied().flatten(),
                });
            }
            rows.extend(meta);
        }
    }
    rows
}
pub fn body_rows(mode: DiffLayout, file_index: u32, file: &FileDiff) -> Vec<DiffRow> {
    if mode == DiffLayout::Unified {
        return super::body_rows(file_index, file);
    }
    let mut rows = Vec::new();
    for notice in 0..super::file_notices(file).len() {
        rows.push(DiffRow::Notice {
            file: file_index,
            notice: notice as u32,
        });
    }
    for (hunk, h) in file.hunks.iter().enumerate() {
        rows.push(DiffRow::HunkHeader {
            file: file_index,
            hunk: hunk as u32,
        });
        rows.extend(align(&h.lines).into_iter().map(|p| DiffRow::SplitLine {
            file: file_index,
            hunk: hunk as u32,
            old: p.old,
            new: p.new,
        }));
    }
    rows.push(DiffRow::BodyPad { file: file_index });
    rows
}
pub fn flatten(
    mode: DiffLayout,
    files: &[FileDiff],
    mut collapsed: impl FnMut(usize) -> bool,
) -> (Vec<DiffRow>, Vec<std::ops::Range<usize>>) {
    let mut rows = Vec::new();
    let mut ranges = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let start = rows.len();
        rows.push(DiffRow::FileHeader { file: i as u32 });
        if !collapsed(i) {
            rows.extend(body_rows(mode, i as u32, file));
        }
        ranges.push(start..rows.len());
    }
    (rows, ranges)
}
pub fn file_index(row: DiffRow) -> u32 {
    match row {
        DiffRow::FileHeader { file }
        | DiffRow::Notice { file, .. }
        | DiffRow::HunkHeader { file, .. }
        | DiffRow::Line { file, .. }
        | DiffRow::SplitLine { file, .. }
        | DiffRow::BodyPad { file }
        | DiffRow::FoldingBody { file } => file,
    }
}
pub fn relocate(row: DiffRow, rows: &[DiffRow]) -> Option<usize> {
    let source = match row {
        DiffRow::Line {
            file, hunk, line, ..
        } => Some((file, hunk, line)),
        DiffRow::SplitLine {
            file,
            hunk,
            old,
            new,
        } => old.or(new).map(|line| (file, hunk, line)),
        _ => None,
    };
    rows.iter()
        .position(|r| {
            if *r == row {
                return true;
            }
            match (source, *r) {
                (
                    Some((f, h, l)),
                    DiffRow::Line {
                        file, hunk, line, ..
                    },
                ) => f == file && h == hunk && l == line,
                (
                    Some((f, h, l)),
                    DiffRow::SplitLine {
                        file,
                        hunk,
                        old,
                        new,
                    },
                ) => f == file && h == hunk && (old == Some(l) || new == Some(l)),
                _ => false,
            }
        })
        .or_else(|| {
            rows.iter().position(|r| {
                *r == DiffRow::FileHeader {
                    file: file_index(row),
                }
            })
        })
}

pub fn selected_file<'a>(
    owner: &str,
    keys: impl IntoIterator<Item = &'a str>,
    files: &[FileDiff],
    side: Option<Side>,
) -> Option<String> {
    let prefix = format!("{owner}:f");
    let index = |key: &str| {
        key.strip_prefix(&prefix)?
            .split_once(":h")?
            .0
            .parse::<usize>()
            .ok()
    };
    let mut keys = keys.into_iter();
    let file = index(keys.next()?)?;
    for key in keys {
        if index(key) != Some(file) {
            return None;
        }
    }
    let file = files.get(file)?;
    Some(if side == Some(Side::Old) {
        file.old_path.as_ref().unwrap_or(&file.path).clone()
    } else {
        file.path.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lines(kinds: &[LineKind]) -> Vec<DiffLine> {
        kinds
            .iter()
            .enumerate()
            .map(|(i, k)| DiffLine {
                kind: *k,
                old_no: None,
                new_no: None,
                text: format!("line {i}"),
            })
            .collect()
    }
    #[test]
    fn replacements_pad_without_reordering_either_side() {
        use LineKind::*;
        let l = lines(&[Context, Del, Del, Add, Context, Add, Add]);
        assert_eq!(
            align(&l),
            vec![
                Pair {
                    old: Some(0),
                    new: Some(0)
                },
                Pair {
                    old: Some(1),
                    new: Some(3)
                },
                Pair {
                    old: Some(2),
                    new: None
                },
                Pair {
                    old: Some(4),
                    new: Some(4)
                },
                Pair {
                    old: None,
                    new: Some(5)
                },
                Pair {
                    old: None,
                    new: Some(6)
                },
            ]
        );
    }
    #[test]
    fn newline_markers_remain_metadata_on_the_right_side() {
        use LineKind::*;
        let l = lines(&[Del, Meta, Add, Meta, Context, Meta]);
        assert_eq!(
            align(&l),
            vec![
                Pair {
                    old: Some(0),
                    new: Some(2)
                },
                Pair {
                    old: Some(1),
                    new: None
                },
                Pair {
                    old: None,
                    new: Some(3)
                },
                Pair {
                    old: Some(4),
                    new: Some(4)
                },
                Pair {
                    old: Some(5),
                    new: Some(5)
                },
            ]
        );
    }
    #[test]
    fn layout_storage_is_separate_and_defaults_safely() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("ui-settings.json"), b"keep").unwrap();
        assert_eq!(load(d.path()), DiffLayout::Unified);
        save(d.path(), DiffLayout::Split).unwrap();
        assert_eq!(load(d.path()), DiffLayout::Split);
        assert_eq!(
            std::fs::read(d.path().join("ui-settings.json")).unwrap(),
            b"keep"
        );
        std::fs::write(d.path().join(FILE_NAME), b"broken").unwrap();
        assert_eq!(load(d.path()), DiffLayout::Unified);
    }

    #[test]
    fn each_side_preserves_source_order_and_does_not_copy_padding() {
        use LineKind::*;
        for deleted in 0..9 {
            for added in 0..9 {
                let mut kinds = vec![Context];
                kinds.extend(std::iter::repeat_n(Del, deleted));
                kinds.extend(std::iter::repeat_n(Add, added));
                kinds.push(Context);
                let source = lines(&kinds);
                let pairs = align(&source);
                let left: Vec<_> = pairs.iter().filter_map(|p| p.old).collect();
                let right: Vec<_> = pairs.iter().filter_map(|p| p.new).collect();
                assert_eq!(
                    left,
                    kinds
                        .iter()
                        .enumerate()
                        .filter(|(_, k)| **k != Add)
                        .map(|(i, _)| i as u32)
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    right,
                    kinds
                        .iter()
                        .enumerate()
                        .filter(|(_, k)| **k != Del)
                        .map(|(i, _)| i as u32)
                        .collect::<Vec<_>>()
                );
                assert!(pairs.iter().all(|p| p.old.is_some() || p.new.is_some()));
            }
        }
    }

    fn files() -> Vec<FileDiff> {
        crate::changes::parse_patch(
            "diff --git a/x.rs b/x.rs\n@@ -1,3 +1,3 @@\n start\n-old\n+new\n end\ndiff --git a/y b/y\nnew file mode 100644\n@@ -0,0 +1,1 @@\n+created\n",
        )
    }

    #[test]
    fn projection_keeps_file_ranges_folds_and_reading_anchor() {
        let files = files();
        let (unified, _) = flatten(DiffLayout::Unified, &files, |_| false);
        assert_eq!(
            flatten(DiffLayout::Unified, &files, |_| false),
            crate::changes::flatten_rows(&files, |_| false)
        );
        let (split, ranges) = flatten(DiffLayout::Split, &files, |i| i == 1);
        assert_eq!(ranges[1].len(), 1);
        assert_eq!(ranges[0].end, ranges[1].start);
        for row in unified.iter().filter(|row| file_index(**row) == 0) {
            let at = relocate(*row, &split).unwrap();
            if let DiffRow::Line { line, .. } = row {
                assert!(
                    matches!(split[at],DiffRow::SplitLine{old,new,..} if old==Some(*line)||new==Some(*line))
                );
            }
        }
        let old = unified
            .iter()
            .find(|r| matches!(r, DiffRow::Line { file: 1, .. }))
            .unwrap();
        assert_eq!(
            split[relocate(*old, &split).unwrap()],
            DiffRow::FileHeader { file: 1 }
        );
    }

    #[test]
    fn copying_a_column_uses_only_that_versions_source_text() {
        use crate::markdown::selection::resolve_spans;
        let f = files();
        let h = &f[0].hunks[0];
        let pairs = align(&h.lines);
        for (side, expected) in [
            (Side::Old, "start\nold\nend"),
            (Side::New, "start\nnew\nend"),
        ] {
            let source: Vec<_> = pairs
                .iter()
                .filter_map(|p| match side {
                    Side::Old => p.old,
                    Side::New => p.new,
                })
                .map(|i| (format!("line-{i}"), h.lines[i as usize].text.clone()))
                .collect();
            let entries: Vec<_> = source
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let last = entries.len() - 1;
            let spans = resolve_spans(&entries, (0, 0), (last, entries[last].1.len()));
            let quote = spans
                .iter()
                .map(|s| &s.text[s.range.clone()])
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(quote, expected);
        }
    }

    #[test]
    fn rename_selection_uses_the_correct_path_and_rejects_mixed_files() {
        let mut files = files();
        files[0].old_path = Some("before.rs".into());
        assert_eq!(
            selected_file("pane", ["pane:f0:h0:l1:old"], &files, Some(Side::Old)),
            Some("before.rs".into())
        );
        assert_eq!(
            selected_file("pane", ["pane:f0:h0:l2:new"], &files, Some(Side::New)),
            Some("x.rs".into())
        );
        assert_eq!(
            selected_file(
                "pane",
                ["pane:f0:h0:l1:old", "pane:f1:h0:l0:old"],
                &files,
                Some(Side::Old)
            ),
            None
        );
        assert_eq!(
            selected_file("pane", ["another:f0:h0:l0"], &files, None),
            None
        );
    }

    #[test]
    fn large_replacements_materialize_paired_rows_not_source_copies() {
        let mut source = lines(&vec![LineKind::Del; 10000]);
        source.extend(lines(&vec![LineKind::Add; 10000]));
        let paired = align(&source);
        assert_eq!(paired.len(), 10000);
        assert_eq!(
            paired.last(),
            Some(&Pair {
                old: Some(9999),
                new: Some(19999)
            })
        );
    }

    #[test]
    fn failed_save_keeps_other_files_and_cleans_staging() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(FILE_NAME)).unwrap();
        assert!(save(d.path(), DiffLayout::Split).is_err());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
